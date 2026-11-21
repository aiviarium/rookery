// Machines and provisioners: a provisioner entry names an engine a plugin drives,
// and what it makes are resources. The core knows one kind, machine, which is a
// host's; everything else is the plugin's.
//
// The engine's health, the actions a type declares and the machines an entry made
// come from ProvisionerService; an action runs as the job whose log follows it.
import { For, Show, createEffect, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import type { ProvisionerEntry, ProvisionerResource, ProvisionerType } from "@rookery/proto";
import { Badge, Button, Card, EmptyState, ListRow, RowList, Skeleton, State, TabPanel, Tabs } from "@aiviarium/nocturne";

import { createAction } from "../../../parts/action.ts";
import { createLive } from "../../../parts/data.ts";
import { ConfirmDialog } from "../../../parts/dialogs.tsx";
import { RowActions } from "../../../parts/row-actions.tsx";
import { createTab } from "../../../parts/tab.ts";
import { View } from "../../../parts/view.tsx";
import { useShell } from "../../../shell.tsx";
import { dateOf, errorMessage, machineAddress, relativeTime } from "../format.ts";
import { NewMachine, NewProvisioner, RunProvisionerAction, engineName } from "./dialogs.tsx";

const TABS = ["machines", "provisioners"] as const;
type Tab = (typeof TABS)[number];

type Health = Record<string, { up: boolean; message: string }>;

export function MachinesPage(): JSX.Element {
  const shell = useShell();
  const [tab, setTab] = createTab<Tab>(TABS);
  const types = createLive(() => shell.rookery.provisioners.listTypes({}));
  const entries = createLive(() => shell.rookery.provisioners.list({ limit: 200 }));
  const machines = createLive(() => shell.rookery.provisioners.listMachines({ limit: 200 }), { events: ["rookery/resources."] });
  const hosts = createLive(() => shell.rookery.hosts.list({ limit: 500 }));
  const [health, setHealth] = createSignal<Health>({});
  const [addingProvisioner, setAddingProvisioner] = createSignal(false);
  const [addingMachine, setAddingMachine] = createSignal<{ entry?: string }>();
  const [acting, setActing] = createSignal<ProvisionerEntry>();
  const [removingEntry, setRemovingEntry] = createSignal<ProvisionerEntry>();
  const [removingMachine, setRemovingMachine] = createSignal<ProvisionerResource>();

  const entryRows = () => entries()?.provisioners ?? [];
  const machineRows = () => [...(machines()?.machines ?? [])].sort((a, b) => a.name.localeCompare(b.name));
  const typeOf = (entry: ProvisionerEntry): ProvisionerType | undefined => types()?.types.find((one) => one.id === entry.type);

  // Each engine's health, read again when the entries change.
  createEffect(
    () => entryRows().map((entry) => entry.name),
    (names) => {
      let stale = false;
      void Promise.all(
        names.map(async (name) => {
          const status = await shell.rookery.provisioners.status({ provisioner: name }).catch(() => undefined);
          return [name, { up: status?.up ?? false, message: status?.message ?? "" }] as const;
        }),
      ).then((pairs) => {
        if (!stale) setHealth(Object.fromEntries(pairs));
      });
      return () => {
        stale = true;
      };
    },
  );

  const refresh = () => {
    void entries.refresh().catch(() => undefined);
    void machines.refresh().catch(() => undefined);
  };
  const removeEntry = createAction(async () => {
    const entry = removingEntry();
    if (entry !== undefined) await shell.rookery.provisioners.remove({ provisioner: entry.name });
    refresh();
  }, { done: "Provisioner removed" });
  const removeMachine = createAction(async () => {
    const machine = removingMachine();
    if (machine === undefined) return;
    const removed = await shell.rookery.provisioners.removeMachine({ host: machine.name });
    if (removed.job !== "") shell.navigate(`/jobs/${encodeURIComponent(removed.job)}`);
    else refresh();
  });

  const entryMenu = (entry: ProvisionerEntry) => [
    { id: "machine", label: "New machine", icon: "plus" as const },
    { id: "action", label: "Run an action", icon: "play" as const, disabled: (typeOf(entry)?.actions.length ?? 0) === 0 },
    { type: "separator" as const },
    { id: "remove", label: "Remove", icon: "trash" as const, danger: true },
  ];
  const chooseEntry = (entry: ProvisionerEntry, id: string) => {
    if (id === "machine") setAddingMachine({ entry: entry.name });
    if (id === "action") setActing(entry);
    if (id === "remove") setRemovingEntry(entry);
  };
  const machineMenu = [{ id: "remove", label: "Remove machine", icon: "trash" as const, danger: true }];

  const failure = () => entries.error() ?? machines.error();

  return (
    <View
      id="machines"
      crumbs={[{ label: "Machines" }]}
      error={failure() === undefined ? undefined : `The machines could not be read. ${errorMessage(failure())}`}
      actions={[
        { id: "refresh", label: "Refresh", icon: "refresh-cw", onClick: refresh },
        tab() === "machines"
          ? {
              id: "machine",
              label: "New machine",
              icon: "plus",
              primary: true,
              disabled: entryRows().length === 0,
              onClick: () => setAddingMachine({}),
            }
          : { id: "provisioner", label: "Add provisioner", icon: "plus", primary: true, onClick: () => setAddingProvisioner(true) },
      ]}
    >
      <Tabs
        variant="underline"
        label="Machines"
        value={tab()}
        onChange={(next) => setTab(next as Tab)}
        items={[
          { id: "machines", label: "Machines", count: machines() === undefined ? undefined : machineRows().length },
          { id: "provisioners", label: "Provisioners", count: entries() === undefined ? undefined : entryRows().length },
        ]}
      >
        <TabPanel id="machines">
          <Show when={machines()} fallback={<Skeleton width="50%" />}>
            <Show
              when={machineRows().length > 0}
              fallback={
                <Card>
                  <EmptyState
                    icon="server"
                    title="No machines yet"
                    description={
                      entryRows().length === 0
                        ? "A provisioner makes machines, such as VMs or containers. Add one first."
                        : "A machine a provisioner makes becomes a host, and its first apply sets it up."
                    }
                  >
                    <Show
                      when={entryRows().length > 0}
                      fallback={
                        <Button variant="primary" icon="plus" onClick={() => (setTab("provisioners"), setAddingProvisioner(true))}>
                          Add provisioner
                        </Button>
                      }
                    >
                      <Button variant="primary" icon="plus" onClick={() => setAddingMachine({})}>
                        New machine
                      </Button>
                    </Show>
                  </EmptyState>
                </Card>
              }
            >
              <Card padding="none">
                <RowList label="Machines">
                  <For each={machineRows()} keyed={(machine) => machine.id}>
                    {(machine) => (
                      <ListRow
                        data-machine={machine().name}
                        icon="server"
                        title={machine().name}
                        secondary={<span class="nc-mono">{machineAddress(machine()) || "no address yet"}</span>}
                        meta={
                          <span class="nc-row nc-row--tight">
                            <Badge tone="outline">{machine().provisioner}</Badge>
                            <span>{relativeTime(dateOf(machine().createdAt))}</span>
                          </span>
                        }
                        href={`/hosts/${encodeURIComponent(machine().name)}`}
                        menu={machineMenu}
                        onMenuSelect={() => setRemovingMachine(machine())}
                        persistentActions={
                          <RowActions label={`Actions for ${machine().name}`} items={machineMenu} onSelect={() => setRemovingMachine(machine())} />
                        }
                      />
                    )}
                  </For>
                </RowList>
              </Card>
            </Show>
          </Show>
        </TabPanel>
        <TabPanel id="provisioners">
          <Show when={entries()} fallback={<Skeleton width="50%" />}>
            <Show
              when={entryRows().length > 0}
              fallback={
                <Card>
                  <EmptyState
                    icon="layers"
                    title="No provisioners"
                    description="A provisioner names a type an installed plugin declares, such as Docker or Incus, and the engine it drives."
                  >
                    <Button variant="primary" icon="plus" onClick={() => setAddingProvisioner(true)}>
                      Add provisioner
                    </Button>
                  </EmptyState>
                </Card>
              }
            >
              <Card padding="none">
                <RowList label="Provisioners">
                  <For each={entryRows()} keyed={(entry) => entry.name}>
                    {(entry) => {
                      const up = () => health()[entry().name];
                      return (
                        <ListRow
                          data-provisioner={entry().name}
                          icon="layers"
                          title={entry().name}
                          secondary={<span class="nc-mono">{`${entry().type} · ${engineName(entry())}`}</span>}
                          detail={up() !== undefined && !up()!.up && up()!.message !== "" ? up()!.message : undefined}
                          status={
                            <Show when={up()} fallback={<State status="off">Checking</State>}>
                              {(known) => <State status={known().up ? "ok" : "error"}>{known().up ? "Up" : "Down"}</State>}
                            </Show>
                          }
                          meta={
                            <span class="nc-row nc-row--tight">
                              <For each={typeOf(entry())?.kinds ?? []}>{(kind) => <Badge tone="outline">{kind.label || kind.id}</Badge>}</For>
                            </span>
                          }
                          menu={entryMenu(entry())}
                          onMenuSelect={(id) => chooseEntry(entry(), id)}
                          persistentActions={
                            <RowActions label={`Actions for ${entry().name}`} items={entryMenu(entry())} onSelect={(id) => chooseEntry(entry(), id)} />
                          }
                        />
                      );
                    }}
                  </For>
                </RowList>
              </Card>
            </Show>
          </Show>
        </TabPanel>
      </Tabs>

      <Show when={addingProvisioner()}>
        <NewProvisioner
          types={types()?.types ?? []}
          hosts={(hosts()?.hosts ?? []).map((host) => host.name)}
          onClose={() => setAddingProvisioner(false)}
          onAdded={() => {
            setAddingProvisioner(false);
            refresh();
          }}
        />
      </Show>
      <Show when={addingMachine()}>
        {(adding) => (
          <NewMachine
            entries={entryRows()}
            types={types()?.types ?? []}
            entry={adding().entry}
            onClose={() => setAddingMachine(undefined)}
            onStarted={(job) => {
              setAddingMachine(undefined);
              shell.navigate(`/jobs/${encodeURIComponent(job)}`);
            }}
          />
        )}
      </Show>
      <Show when={acting()} keyed>
        {(entry) => (
          <RunProvisionerAction
            entry={entry}
            type={typeOf(entry)}
            onClose={() => setActing(undefined)}
            onStarted={(job) => {
              setActing(undefined);
              shell.navigate(`/jobs/${encodeURIComponent(job)}`);
            }}
          />
        )}
      </Show>
      <ConfirmDialog
        open={removingEntry() !== undefined}
        title={`Remove ${removingEntry()?.name ?? "the provisioner"}?`}
        description="The machines it made stay as hosts, and it makes no new ones."
        confirm="Remove provisioner"
        danger
        action={removeEntry}
        onClose={() => setRemovingEntry(undefined)}
      />
      <ConfirmDialog
        open={removingMachine() !== undefined}
        title={`Remove ${removingMachine()?.name ?? "the machine"}?`}
        description="The provisioner destroys the machine and its disks, and the host goes with it."
        confirm="Remove machine"
        danger
        action={removeMachine}
        onClose={() => setRemovingMachine(undefined)}
      />
    </View>
  );
}

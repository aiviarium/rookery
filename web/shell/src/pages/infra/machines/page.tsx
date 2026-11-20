// Machines and provisioners: a provisioner entry names an engine a plugin drives,
// and what it makes are resources. The core knows one kind, machine, which is a
// host's; everything else is the plugin's.
//
// The engine's health, the actions a type declares and the machines an entry made
// come from ProvisionerService; an action runs as the job whose log follows it.
import { For, Show, createEffect, createSignal, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import type { ProvisionerEntry, ProvisionerResource, ProvisionerType } from "@rookery/proto";

import { useShell } from "../../../shell.tsx";
import { Badge, Button, Card, IconButton, Page, Spinner } from "../../../ui.tsx";
import { machineAddress, relativeTime, dateOf } from "../format.ts";
import { StateText, Table, type Column } from "../parts.tsx";
import { NewMachine, NewProvisioner, RunProvisionerAction, engineName } from "./dialogs.tsx";

export function MachinesPage(): JSX.Element {
  const shell = useShell();
  const types = createQuery(() => shell.rookery.provisioners.listTypes({}));
  const entries = createQuery(() => shell.rookery.provisioners.list({ limit: 200 }));
  const hosts = createQuery(() => shell.rookery.hosts.list({ limit: 200 }));
  const [addingProvisioner, setAddingProvisioner] = createSignal(false);
  const [addingMachine, setAddingMachine] = createSignal(false);
  const [forEntry, setForEntry] = createSignal<string>();
  const [acting, setActing] = createSignal<ProvisionerEntry>();

  const made = createQuery(async () => {
    const listed = entries()?.provisioners ?? [];
    const [resources, health] = await Promise.all([
      shell.rookery.provisioners.listMachines({ limit: 200 }).catch(() => undefined),
      Promise.all(
        listed.map(async entry => {
          const status = await shell.rookery.provisioners.status({ provisioner: entry.name }).catch(() => undefined);
          return [entry.name, { up: status?.up ?? false, message: status?.message ?? "" }] as const;
        }),
      ),
    ]);
    return {
      machines: [...(resources?.machines ?? [])].sort((a, b) => a.name.localeCompare(b.name)),
      health: Object.fromEntries(health) as Record<string, { up: boolean; message: string }>,
    };
  });

  const machines = (): ProvisionerResource[] => made()?.machines ?? [];
  const health = (): Record<string, { up: boolean; message: string }> => made()?.health ?? {};

  createEffect(
    () => entries()?.provisioners.map(entry => entry.name).join(","),
    () => void made.refresh().catch(() => undefined),
  );

  onSettled(() =>
    shell.connection.on("rookery/resources.changed", () => {
      void made.refresh().catch(() => undefined);
    }),
  );

  const typeOf = (entry: ProvisionerEntry): ProvisionerType | undefined =>
    types()?.types.find(one => one.id === entry.type);

  const removeEntry = async (entry: ProvisionerEntry): Promise<void> => {
    try {
      await shell.rookery.provisioners.remove({ provisioner: entry.name });
      void entries.refresh().catch(() => undefined);
      void made.refresh().catch(() => undefined);
    } catch {
      // The list below says the entry is still there; the next read is the truth.
    }
  };

  const removeMachine = async (machine: ProvisionerResource): Promise<void> => {
    try {
      const removed = await shell.rookery.provisioners.removeMachine({ host: machine.name });
      if (removed.job !== "") {
        shell.navigate(`/jobs/${encodeURIComponent(removed.job)}`);
        return;
      }
      void made.refresh().catch(() => undefined);
    } catch {
      // The list below says the machine is still there; the next read is the truth.
    }
  };

  const entryColumns: Column<ProvisionerEntry>[] = [
    { id: "name", header: "Name", mono: true, value: entry => entry.name },
    { id: "type", header: "Type", mono: true, value: entry => entry.type },
    { id: "engine", header: "Engine", mono: true, value: entry => engineName(entry) },
    {
      id: "kinds",
      header: "Makes",
      value: entry => (typeOf(entry)?.kinds ?? []).map(kind => kind.id).join(", "),
      cell: entry => (
        <span class="nc-row">
          <For each={typeOf(entry)?.kinds ?? []}>
            {kind => <Badge tone="outline">{kind.label === "" ? kind.id : kind.label}</Badge>}
          </For>
        </span>
      ),
    },
    {
      id: "health",
      header: "Engine",
      value: entry => (health()[entry.name]?.up === true ? "Up" : "Down"),
      cell: entry => (
        <StateText tone={health()[entry.name]?.up === true ? "ok" : "off"}>
          {health()[entry.name]?.up === true ? "Up" : "Down"}
        </StateText>
      ),
    },
  ];

  const machineColumns: Column<ProvisionerResource>[] = [
    { id: "name", header: "Name", mono: true, value: machine => machine.name },
    { id: "provisioner", header: "Provisioner", mono: true, value: machine => machine.provisioner },
    { id: "address", header: "Address", mono: true, value: machine => machineAddress(machine) },
    { id: "made", header: "Made", value: machine => relativeTime(dateOf(machine.createdAt)) },
  ];

  return (
    <Page
      title="Machines"
      description="A provisioner makes a machine, which is a host; the apply that follows sets it up."
      actions={
        <>
          <IconButton label="Read the provisioners again" glyph="server" onClick={() => void entries.refresh()} />
          <Button onClick={() => setAddingProvisioner(true)}>Add provisioner</Button>
          <Button
            variant="primary"
            disabled={(entries()?.provisioners.length ?? 0) === 0}
            onClick={() => {
              setForEntry(undefined);
              setAddingMachine(true);
            }}
          >
            New machine
          </Button>
        </>
      }
    >
      <Card title="Provisioners">
        <p class="nc-meta">{entries()?.provisioners.length ?? 0} {entries()?.provisioners.length === 1 ? "provisioner" : "provisioners"}</p>
          <Table
            label="Provisioners"
            rows={entries()?.provisioners ?? []}
            columns={entryColumns}
            rowKey={entry => entry.name}
            rowActions={entry => (
              <span class="nc-row">
                <Button
                  size="sm"
                  onClick={() => {
                    setForEntry(entry.name);
                    setAddingMachine(true);
                  }}
                >
                  New machine
                </Button>
                <Button size="sm" disabled={(typeOf(entry)?.actions.length ?? 0) === 0} onClick={() => setActing(entry)}>
                  Action
                </Button>
                <Button size="sm" variant="danger" onClick={() => void removeEntry(entry)}>
                  Remove
                </Button>
              </span>
            )}
          />
      </Card>
      <Card title="Machines">
        <p class="nc-meta">{machines().length} {machines().length === 1 ? "machine" : "machines"}</p>
        <Show when={!entries.pending()} fallback={<Spinner />}>
          <Table
            label="Machines"
            rows={machines()}
            columns={machineColumns}
            rowKey={machine => machine.id}
            onRowClick={machine => shell.navigate(`/hosts/${encodeURIComponent(machine.name)}`)}
            rowActions={machine => (
              <Button
                size="sm"
                variant="danger"
                onClick={event => {
                  event.stopPropagation();
                  void removeMachine(machine);
                }}
              >
                Remove
              </Button>
            )}
            empty="No machine yet. A machine a provisioner made is a host, and an entry here."
          />
        </Show>
      </Card>
      <Show when={addingProvisioner()}>
        <NewProvisioner
          types={types()?.types ?? []}
          hosts={(hosts()?.hosts ?? []).map(host => host.name)}
          onClose={() => setAddingProvisioner(false)}
          onAdded={() => {
            setAddingProvisioner(false);
            void entries.refresh().catch(() => undefined);
          }}
        />
      </Show>
      <Show when={addingMachine()}>
        <NewMachine
          entries={entries()?.provisioners ?? []}
          types={types()?.types ?? []}
          entry={forEntry()}
          onClose={() => setAddingMachine(false)}
          onStarted={job => {
            setAddingMachine(false);
            shell.navigate(`/jobs/${encodeURIComponent(job)}`);
          }}
        />
      </Show>
      <Show when={acting()}>
        {entry => (
          <RunProvisionerAction
            entry={entry()}
            type={typeOf(entry())}
            onClose={() => setActing(undefined)}
            onStarted={job => {
              setActing(undefined);
              shell.navigate(`/jobs/${encodeURIComponent(job)}`);
            }}
          />
        )}
      </Show>
    </Page>
  );
}

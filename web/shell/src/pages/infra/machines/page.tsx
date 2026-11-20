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
import { Badge, Button, Card, DataTable, type DataColumn, Skeleton, State } from "@aiviarium/nocturne";

import { useShell } from "../../../shell.tsx";
import { Page } from "../../../parts/page.tsx";
import { machineAddress, relativeTime, dateOf } from "../format.ts";
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

  const entryColumns = (): DataColumn<ProvisionerEntry>[] => [
    { id: "name", header: "Name", type: "mono", accessor: entry => entry.name },
    { id: "type", header: "Type", type: "mono", accessor: entry => entry.type },
    { id: "engine", header: "Engine", type: "mono", accessor: entry => engineName(entry) },
    {
      id: "kinds",
      header: "Makes",
      accessor: entry => (typeOf(entry)?.kinds ?? []).map(kind => kind.id).join(", "),
      cell: entry => (
        <span class="nc-row nc-row--tight">
          <For each={typeOf(entry)?.kinds ?? []}>
            {kind => <Badge tone="outline">{kind.label === "" ? kind.id : kind.label}</Badge>}
          </For>
        </span>
      ),
    },
    {
      id: "health",
      header: "Health",
      accessor: entry => (health()[entry.name]?.up === true ? "Up" : "Down"),
      cell: entry => (
        <State status={health()[entry.name]?.up === true ? "ok" : "off"}>
          {health()[entry.name]?.up === true ? "Up" : "Down"}
        </State>
      ),
    },
    {
      id: "actions",
      header: "",
      cell: entry => (
        <span class="nc-row nc-row--tight">
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
      ),
    },
  ];

  const machineColumns = (): DataColumn<ProvisionerResource>[] => [
    { id: "name", header: "Name", type: "mono", accessor: machine => machine.name },
    { id: "provisioner", header: "Provisioner", type: "mono", accessor: machine => machine.provisioner },
    { id: "address", header: "Address", type: "mono", accessor: machine => machineAddress(machine) },
    { id: "made", header: "Made", type: "time", accessor: machine => relativeTime(dateOf(machine.createdAt)) },
    {
      id: "actions",
      header: "",
      cell: machine => (
        <Button size="sm" variant="danger" onClick={() => void removeMachine(machine)}>
          Remove
        </Button>
      ),
    },
  ];

  return (
    <Page
      title="Machines"
      description="A provisioner makes a machine, which is a host; the apply that follows sets it up."
      actions={() => [
        { id: "refresh", label: "Read the provisioners again", icon: "refresh-cw", onClick: () => void entries.refresh() },
        { id: "provisioner", label: "Add provisioner", icon: "plus", onClick: () => setAddingProvisioner(true) },
        {
          id: "machine",
          label: "New machine",
          primary: true,
          disabled: (entries()?.provisioners.length ?? 0) === 0,
          onClick: () => {
            setForEntry(undefined);
            setAddingMachine(true);
          },
        },
      ]}
    >
      <Card title="Provisioners" count={entries()?.provisioners.length} padding="none">
        <DataTable
          bare
          label="Provisioners"
          rows={entries()?.provisioners ?? []}
          columns={entryColumns()}
          getRowId={entry => entry.name}
          loading={entries.pending() && entries() === undefined}
          empty={{
            title: "No provisioner",
            description: "An entry names a type an installed plugin declares, and the engine it drives.",
          }}
        />
      </Card>
      <Card title="Machines" count={machines().length} padding="none">
        <Show when={!entries.pending() || entries() !== undefined} fallback={<Skeleton width="40%" />}>
          <DataTable
            bare
            label="Machines"
            rows={machines()}
            columns={machineColumns()}
            getRowId={machine => machine.id}
            loading={made.pending() && made() === undefined}
            onRowClick={machine => shell.navigate(`/hosts/${encodeURIComponent(machine.name)}`)}
            empty={{
              title: "No machine yet",
              description: "A machine a provisioner made is a host, and an entry here.",
            }}
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

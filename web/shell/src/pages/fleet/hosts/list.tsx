// The hosts: every machine Rookery runs, whether it was there already or a
// provisioner made it, and the provisioners that make them. A row opens the
// host; its menu opens a terminal, applies the profile, or removes a machine a
// provisioner made. The list follows the server's host and resource events.
import { Show, createEffect, createSignal, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useSearchParams } from "@solidjs/router";
import type { Host, ProvisionerResource } from "@rookery/proto";
import { Button, Card, type DataColumn, DataTable, EmptyState, Skeleton, State, TabPanel, Tabs } from "@aiviarium/nocturne";

import { createAction } from "../../../parts/action.ts";
import { createLive } from "../../../parts/data.ts";
import { ConfirmDialog } from "../../../parts/dialogs.tsx";
import { errorMessage } from "../../../parts/errors.ts";
import { RowActions } from "../../../parts/row-actions.tsx";
import { createTab } from "../../../parts/tab.ts";
import { View } from "../../../parts/view.tsx";
import { useShell } from "../../../shell.tsx";
import { dateOf, hostIssue, hostTypeLabel, labelsText, relativeTime } from "../format.ts";
import { jobPath } from "../jobs/rows.tsx";
import { ProvisionersPanel } from "../machines/provisioners.tsx";
import { AddHost } from "./add.tsx";
import { HOST_EVENTS, hostPath } from "./paths.ts";

const TABS = ["hosts", "provisioners"] as const;
type Tab = (typeof TABS)[number];

/** Row is a host, the machine a provisioner made for it, or both. */
interface Row {
  readonly name: string;
  readonly host?: Host;
  readonly machine?: ProvisionerResource;
}

export function HostsPage(): JSX.Element {
  const shell = useShell();
  const [search, setSearch] = useSearchParams<{ add?: string }>();
  const [tab, setTab] = createTab<Tab>(TABS);
  const hosts = createLive(() => shell.rookery.hosts.list({ limit: 500 }), { events: HOST_EVENTS });
  const machines = createLive(() => shell.rookery.provisioners.listMachines({ limit: 500 }).catch(() => undefined), {
    events: ["rookery/resources.", ...HOST_EVENTS],
  });
  const entries = createLive(() => shell.rookery.provisioners.list({ limit: 200 }).catch(() => undefined));
  const types = createLive(() => shell.rookery.provisioners.listTypes({}).catch(() => undefined));
  // Home and the sidebar open the dialog with ?add=1, also while the page is open.
  const [adding, setAdding] = createSignal<{ from?: string } | undefined>(untrack(() => (search.add === "1" ? {} : undefined)));
  createEffect(
    () => search.add,
    (add) => {
      if (add === "1") setAdding((open) => open ?? {});
    },
  );
  const [addingProvisioner, setAddingProvisioner] = createSignal(false);
  const [removing, setRemoving] = createSignal<string>();

  const typeList = () => types()?.types ?? [];
  // The provisioners that make machines: the ones "Add host" offers.
  const makers = () =>
    (entries()?.provisioners ?? []).filter((entry) => typeList().find((one) => one.id === entry.type)?.kinds.some((kind) => kind.machine));
  const rows = (): Row[] => {
    const byName = new Map<string, Row>();
    for (const host of hosts()?.hosts ?? []) byName.set(host.name, { name: host.name, host });
    for (const machine of machines()?.machines ?? []) byName.set(machine.name, { ...byName.get(machine.name), name: machine.name, machine });
    return [...byName.values()];
  };

  const refresh = () => {
    void hosts.refresh().catch(() => undefined);
    void machines.refresh().catch(() => undefined);
    void entries.refresh().catch(() => undefined);
  };
  const apply = createAction(
    async (name: string) => {
      const started = await shell.rookery.hosts.apply({ name });
      shell.navigate(jobPath(started.job));
    },
    { failed: "The apply did not start" },
  );
  const removeMachine = createAction(async () => {
    const name = removing();
    if (name === undefined) return;
    const removed = await shell.rookery.provisioners.removeMachine({ host: name });
    if (removed.job !== "") shell.navigate(jobPath(removed.job));
    else refresh();
  });

  const menu = (row: Row | undefined) => [
    { id: "open", label: "Open", icon: "arrow-right" as const },
    { id: "terminal", label: "Terminal", icon: "terminal" as const, disabled: row?.host === undefined },
    { id: "files", label: "Files", icon: "folder" as const, disabled: row?.host === undefined },
    { type: "separator" as const },
    { id: "apply", label: "Apply profile", icon: "play" as const, disabled: row?.host === undefined },
    ...(row?.machine === undefined
      ? []
      : [{ type: "separator" as const }, { id: "remove-machine", label: "Remove machine", icon: "trash" as const, danger: true }]),
  ];
  const choose = (id: string, row: Row | undefined) => {
    if (row === undefined) return;
    if (id === "open") shell.navigate(hostPath(row.name));
    if (id === "terminal") shell.navigate(hostPath(row.name, "terminal"));
    if (id === "files") shell.navigate(hostPath(row.name, "files"));
    if (id === "apply") void apply.run(row.name);
    if (id === "remove-machine") setRemoving(row.name);
  };

  const columns: DataColumn<Row>[] = [
    {
      id: "name",
      header: "Host",
      sortable: true,
      accessor: (row) => row.name,
      cell: (row) => (
        <span class="shell-cell">
          <span class="label">{row.name}</span>
          <span class="nc-meta nc-mono">{row.host?.address || row.host?.profile?.address || row.machine?.address || ""}</span>
        </span>
      ),
    },
    { id: "type", header: "Type", accessor: (row) => (row.host === undefined ? "" : hostTypeLabel(row.host.type)), width: "120px" },
    { id: "machine", header: "Provisioner", accessor: (row) => row.machine?.provisioner ?? "", width: "140px" },
    { id: "labels", header: "Labels", type: "mono", accessor: (row) => (row.host === undefined ? "" : labelsText(row.host.labels)) },
    {
      id: "applied",
      header: "Applied",
      width: "120px",
      sortable: true,
      sortValue: (row) => Number(row.host?.appliedAt?.seconds ?? 0n),
      accessor: (row) => (row.host === undefined ? "" : relativeTime(dateOf(row.host.appliedAt))),
    },
    {
      id: "state",
      header: "",
      width: "120px",
      cell: (row) => {
        if (row.host === undefined) return <State status="idle">Creating</State>;
        const issue = hostIssue(row.host);
        return issue === undefined ? null : <State status={issue.tone}>{issue.label}</State>;
      },
    },
    {
      id: "menu",
      header: "",
      width: "40px",
      cell: (row) => <RowActions label={`Actions for ${row.name}`} items={menu(row)} onSelect={(id) => choose(id, row)} />,
    },
  ];

  const closeAdd = () => {
    setAdding(undefined);
    if (search.add !== undefined) setSearch({ add: undefined }, { replace: true });
  };

  return (
    <View
      id="hosts"
      crumbs={[{ label: "Hosts" }]}
      error={hosts.error() === undefined ? undefined : `The hosts could not be read. ${errorMessage(hosts.error())}`}
      actions={[
        tab() === "hosts"
          ? { id: "add", label: "Add host", icon: "plus", onClick: () => setAdding({}) }
          : { id: "provisioner", label: "Add provisioner", icon: "plus", onClick: () => setAddingProvisioner(true) },
        { id: "refresh", label: "Refresh", icon: "refresh-cw", overflow: true, onClick: refresh },
      ]}
    >
      <Tabs
        variant="underline"
        label="Hosts"
        value={tab()}
        onChange={(next) => setTab(next as Tab)}
        items={[
          { id: "hosts", label: "Hosts", count: rows().length > 0 ? rows().length : undefined },
          {
            id: "provisioners",
            label: "Provisioners",
            count: (entries()?.provisioners.length ?? 0) > 0 ? entries()?.provisioners.length : undefined,
          },
        ]}
      >
        <TabPanel id="hosts">
          <Show when={hosts() !== undefined} fallback={<Skeleton width="50%" />}>
            <Show
              when={rows().length > 0}
              fallback={
                <Card>
                  <EmptyState icon="server" title="No hosts">
                    <Button icon="plus" onClick={() => setAdding({})}>
                      Add host
                    </Button>
                  </EmptyState>
                </Card>
              }
            >
              <Card padding="none">
                <DataTable
                  bare
                  label="Hosts"
                  rows={rows()}
                  columns={columns}
                  getRowId={(row) => row.name}
                  rowLabel={(row) => row.name}
                  onRowClick={(row) => shell.navigate(hostPath(row.name))}
                  rowMenu={(target) => menu(target.rows[0])}
                  onRowMenuSelect={(id, target) => choose(id, target.rows[0])}
                />
              </Card>
            </Show>
          </Show>
        </TabPanel>
        <TabPanel id="provisioners">
          <ProvisionersPanel
            entries={entries()?.provisioners}
            types={typeList()}
            hosts={(hosts()?.hosts ?? []).map((host) => host.name)}
            adding={addingProvisioner()}
            onAdd={() => setAddingProvisioner(true)}
            onCloseAdd={() => setAddingProvisioner(false)}
            onChanged={refresh}
            onNewMachine={(entry) => setAdding({ from: entry })}
          />
        </TabPanel>
      </Tabs>
      <Show when={adding()}>
        {(open) => (
          <AddHost
            entries={makers()}
            types={typeList()}
            from={open().from}
            onClose={closeAdd}
            onAdded={(added) => {
              refresh();
              shell.navigate(added.job === "" ? hostPath(added.name) : jobPath(added.job));
            }}
          />
        )}
      </Show>
      <ConfirmDialog
        open={removing() !== undefined}
        title={`Remove ${removing() ?? "the machine"}?`}
        description="Its disks and its host go with it."
        confirm="Remove machine"
        danger
        action={removeMachine}
        onClose={() => setRemoving(undefined)}
      />
    </View>
  );
}

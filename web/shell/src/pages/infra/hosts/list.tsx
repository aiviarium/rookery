// The hosts: every machine Rookery runs, with its state, its type and its
// labels. The list follows the server's own host events, so it changes as an
// apply finishes or someone else adds a host.
import { Show, createSignal, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import type { Host } from "@rookery/proto";
import { Badge, Card, DataTable, type DataColumn, State } from "@aiviarium/nocturne";

import { useShell } from "../../../shell.tsx";
import { Page } from "../../../parts/page.tsx";
import { hostState, hostTypeLabel, hostTypeTone, labelsText, relativeTime, dateOf } from "../format.ts";
import { AddHost } from "./add.tsx";

/** hostPath is a host's page. */
export function hostPath(name: string): string {
  return `/hosts/${encodeURIComponent(name)}`;
}

/** COLUMNS are what the hosts table shows, in order. */
const COLUMNS: DataColumn<Host>[] = [
  { id: "name", header: "Name", type: "mono", accessor: host => host.name, sortable: true },
  {
    id: "state",
    header: "State",
    accessor: host => hostState(host).label,
    cell: host => <State status={hostState(host).tone}>{hostState(host).label}</State>,
    sortable: true,
  },
  {
    id: "type",
    header: "Type",
    accessor: host => hostTypeLabel(host.type),
    cell: host => <Badge tone={hostTypeTone(host.type)}>{hostTypeLabel(host.type)}</Badge>,
  },
  {
    id: "labels",
    header: "Labels",
    accessor: host => labelsText(host.labels),
    cell: host => <span class="nc-mono nc-meta">{labelsText(host.labels)}</span>,
  },
  {
    id: "address",
    header: "Address",
    type: "mono",
    accessor: host => host.address || host.profile?.address || "",
  },
  {
    id: "applied",
    header: "Applied",
    type: "time",
    accessor: host => relativeTime(dateOf(host.appliedAt)),
  },
];

export function HostsPage(): JSX.Element {
  const shell = useShell();
  const hosts = createQuery(() => shell.rookery.hosts.list({ limit: 200 }));
  const [adding, setAdding] = createSignal(false);

  onSettled(() =>
    shell.connection.on("rookery/hosts.changed", () => {
      void hosts.refresh().catch(() => undefined);
    }),
  );

  const rows = () => hosts()?.hosts ?? [];

  return (
    <Page
      title="Hosts"
      description="The machines Rookery runs: reached over SSH, set up with a profile, and used through terminals, files, ports and desktops."
      actions={() => [
        {
          id: "refresh",
          label: "Read the hosts again",
          icon: "refresh-cw",
          onClick: () => void hosts.refresh(),
        },
        { id: "add", label: "Add host", icon: "plus", onClick: () => setAdding(true) },
      ]}
    >
      <Card title="Hosts" count={rows().length} padding="none">
        <DataTable
          bare
          label="Hosts"
          rows={rows()}
          columns={COLUMNS}
          getRowId={host => host.name}
          loading={hosts.pending() && hosts() === undefined}
          onRowClick={host => shell.navigate(hostPath(host.name))}
          empty={{
            title: "No hosts yet",
            description: "Add a machine Rookery reaches over SSH, and apply its profile to make it a managed host or a workstation.",
          }}
          error={
            hosts.error() === undefined
              ? undefined
              : { message: "The hosts could not be read.", detail: hosts.error()?.message, onRetry: () => void hosts.refresh() }
          }
        />
      </Card>
      <Show when={adding()}>
        <AddHost
          onClose={() => setAdding(false)}
          onAdded={(host, firstContact) => {
            setAdding(false);
            void hosts.refresh().catch(() => undefined);
            shell.navigate(firstContact === "" ? hostPath(host.name) : `/jobs/${encodeURIComponent(firstContact)}`);
          }}
        />
      </Show>
    </Page>
  );
}

// The hosts: every machine Rookery runs. A row opens the host; its menu opens a
// terminal or applies the profile. The list follows the server's host events,
// so it changes as an apply finishes or someone else adds a host.
import { Show, createSignal, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useSearchParams } from "@solidjs/router";
import type { Host } from "@rookery/proto";
import { Button, Card, type DataColumn, DataTable, EmptyState, Skeleton, State } from "@aiviarium/nocturne";

import { createAction } from "../../../parts/action.ts";
import { createLive } from "../../../parts/data.ts";
import { errorMessage } from "../../../parts/errors.ts";
import { View } from "../../../parts/view.tsx";
import { useShell } from "../../../shell.tsx";
import { dateOf, hostIssue, hostTypeLabel, labelsText, relativeTime } from "../format.ts";
import { jobPath } from "../jobs/rows.tsx";
import { AddHost } from "./add.tsx";
import { HOST_EVENTS, hostPath } from "./paths.ts";

export function HostsPage(): JSX.Element {
  const shell = useShell();
  const [search, setSearch] = useSearchParams<{ add?: string }>();
  const hosts = createLive(() => shell.rookery.hosts.list({ limit: 500 }), { events: HOST_EVENTS });
  // Home and the sidebar open the dialog with ?add=1.
  const [adding, setAdding] = createSignal(untrack(() => search.add === "1"));
  const rows = () => hosts()?.hosts ?? [];
  const apply = createAction(
    async (name: string) => {
      const started = await shell.rookery.hosts.apply({ name });
      shell.navigate(jobPath(started.job));
    },
    { failed: "The apply did not start" },
  );

  const columns: DataColumn<Host>[] = [
    {
      id: "name",
      header: "Host",
      sortable: true,
      accessor: (host) => host.name,
      cell: (host) => (
        <span class="shell-cell">
          <span class="label">{host.name}</span>
          <span class="nc-meta nc-mono">{host.address || host.profile?.address || ""}</span>
        </span>
      ),
    },
    { id: "type", header: "Type", accessor: (host) => hostTypeLabel(host.type), width: "120px" },
    { id: "labels", header: "Labels", type: "mono", accessor: (host) => labelsText(host.labels) },
    {
      id: "applied",
      header: "Applied",
      width: "120px",
      sortable: true,
      sortValue: (host) => Number(host.appliedAt?.seconds ?? 0n),
      accessor: (host) => relativeTime(dateOf(host.appliedAt)),
    },
    {
      id: "state",
      header: "",
      width: "120px",
      cell: (host) => {
        const issue = hostIssue(host);
        return issue === undefined ? null : <State status={issue.tone}>{issue.label}</State>;
      },
    },
  ];

  const close = () => {
    setAdding(false);
    if (search.add !== undefined) setSearch({ add: undefined }, { replace: true });
  };

  return (
    <View
      id="hosts"
      crumbs={[{ label: "Hosts" }]}
      error={hosts.error() === undefined ? undefined : `The hosts could not be read. ${errorMessage(hosts.error())}`}
      actions={[{ id: "add", label: "Add host", icon: "plus", onClick: () => setAdding(true) }]}
    >
      <Show when={hosts() !== undefined} fallback={<Skeleton width="50%" />}>
        <Show
          when={rows().length > 0}
          fallback={
            <Card>
              <EmptyState icon="server" title="No hosts">
                <Button icon="plus" onClick={() => setAdding(true)}>
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
              getRowId={(host) => host.name}
              rowLabel={(host) => host.name}
              onRowClick={(host) => shell.navigate(hostPath(host.name))}
              rowMenu={() => [
                { id: "open", label: "Open", icon: "arrow-right" },
                { id: "terminal", label: "Terminal", icon: "terminal" },
                { id: "files", label: "Files", icon: "folder" },
                { type: "separator" },
                { id: "apply", label: "Apply profile", icon: "play" },
              ]}
              onRowMenuSelect={(id, target) => {
                const host = target.rows[0];
                if (host === undefined) return;
                if (id === "open") shell.navigate(hostPath(host.name));
                if (id === "terminal") shell.navigate(hostPath(host.name, "terminal"));
                if (id === "files") shell.navigate(hostPath(host.name, "files"));
                if (id === "apply") void apply.run(host.name);
              }}
            />
          </Card>
        </Show>
      </Show>
      <Show when={adding()}>
        <AddHost
          onClose={close}
          onAdded={(name, firstContact) => {
            void hosts.refresh().catch(() => undefined);
            shell.navigate(firstContact === "" ? hostPath(name) : jobPath(firstContact));
          }}
        />
      </Show>
    </View>
  );
}

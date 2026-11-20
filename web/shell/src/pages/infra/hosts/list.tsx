// The hosts: every machine Rookery runs, with its state, its type and its
// labels. The list follows the server's own host events, so it changes as an
// apply finishes or someone else adds a host.
import { Show, createSignal, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import type { Host } from "@rookery/proto";

import { useShell } from "../../../shell.tsx";
import { Badge, Button, Callout, IconButton, Page, Spinner } from "../../../ui.tsx";
import { hostState, hostTypeLabel, hostTypeTone, labelsText, relativeTime, dateOf } from "../format.ts";
import { StateText, Table, type Column } from "../parts.tsx";
import { AddHost } from "./add.tsx";

/** hostPath is a host's page. */
export function hostPath(name: string): string {
  return `/hosts/${encodeURIComponent(name)}`;
}

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

  const columns: Column<Host>[] = [
    {
      id: "name",
      header: "Name",
      mono: true,
      value: (host) => host.name,
    },
    {
      id: "state",
      header: "State",
      value: (host) => hostState(host).label,
      cell: (host) => {
        const shown = hostState(host);
        return <StateText tone={shown.tone}>{shown.label}</StateText>;
      },
    },
    {
      id: "type",
      header: "Type",
      value: (host) => hostTypeLabel(host.type),
      cell: (host) => <Badge tone={hostTypeTone(host.type)}>{hostTypeLabel(host.type)}</Badge>,
    },
    {
      id: "labels",
      header: "Labels",
      value: (host) => labelsText(host.labels),
      cell: (host) => <span class="nc-meta nc-code">{labelsText(host.labels)}</span>,
    },
    { id: "address", header: "Address", mono: true, value: (host) => host.address || host.profile?.address || "" },
    { id: "applied", header: "Applied", value: (host) => relativeTime(dateOf(host.appliedAt)) },
  ];

  return (
    <Page
      title="Hosts"
      description="The machines Rookery runs: reached over SSH, set up with a profile, and used through terminals, files, ports and desktops."
      actions={
        <>
          <IconButton label="Read the hosts again" glyph="hosts" onClick={() => void hosts.refresh()} />
          <Button variant="primary" onClick={() => setAdding(true)}>
            Add host
          </Button>
        </>
      }
    >
      <Show when={hosts.error()}>
        {error => (
          <Callout tone="danger">
            The hosts could not be read: {error().message}
          </Callout>
        )}
      </Show>
      <Show
        when={!hosts.pending() || hosts() !== undefined}
        fallback={
          <p class="nc-row">
            <Spinner /> Reading the hosts…
          </p>
        }
      >
        <Show
          when={rows().length > 0}
          fallback={
            <Show when={hosts() !== undefined}>
              <section class="nc-card">
                <div class="nc-pad nc-stack">
                  <p class="nc-empty__title">No hosts yet</p>
                  <p class="nc-empty__desc">
                    Add a machine Rookery reaches over SSH, and apply its profile to make it a managed host or a
                    workstation.
                  </p>
                  <div class="nc-row">
                    <Button onClick={() => setAdding(true)}>Add host</Button>
                  </div>
                </div>
              </section>
            </Show>
          }
        >
          <section class="nc-card">
            <div class="nc-pad nc-stack">
              <div class="nc-row" style={{ "justify-content": "space-between" }}>
                <span class="nc-meta">
                  {rows().length} {rows().length === 1 ? "host" : "hosts"}
                </span>
              </div>
              <Table
                label="Hosts"
                rows={rows()}
                columns={columns}
                rowKey={host => host.name}
                onRowClick={host => shell.navigate(hostPath(host.name))}
              />
            </div>
          </section>
        </Show>
      </Show>
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

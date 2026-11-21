// The hosts: every machine Rookery runs, with its state. Opening one drills
// the sidebar down into it. The list follows the server's host events, so it
// changes as an apply finishes or someone else adds a host.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Badge, Button, Card, EmptyState, ListRow, RowList, Skeleton, State } from "@aiviarium/nocturne";

import { createLive } from "../../../parts/data.ts";
import { errorMessage } from "../../../parts/errors.ts";
import { View } from "../../../parts/view.tsx";
import { useShell } from "../../../shell.tsx";
import { dateOf, hostState, hostTypeLabel, hostTypeTone, labelsText, relativeTime } from "../format.ts";
import { AddHost } from "./add.tsx";
import { HOST_EVENTS, hostPath } from "./paths.ts";

export function HostsPage(): JSX.Element {
  const shell = useShell();
  const hosts = createLive(() => shell.rookery.hosts.list({ limit: 500 }), { events: HOST_EVENTS });
  const [adding, setAdding] = createSignal(false);
  const rows = () => hosts()?.hosts ?? [];

  return (
    <View
      id="hosts"
      crumbs={[{ label: "Hosts" }]}
      error={hosts.error() === undefined ? undefined : `The hosts could not be read. ${errorMessage(hosts.error())}`}
      actions={[
        { id: "refresh", label: "Refresh", icon: "refresh-cw", onClick: () => void hosts.refresh() },
        { id: "add", label: "Add host", icon: "plus", primary: true, onClick: () => setAdding(true) },
      ]}
    >
      <Show when={hosts() !== undefined} fallback={<Skeleton width="50%" />}>
        <Show
          when={rows().length > 0}
          fallback={
            <Card>
              <EmptyState
                icon="server"
                title="No hosts yet"
                description="A host is a machine Rookery reaches over SSH. Applying its profile installs rookeryd and makes it managed."
              >
                <Button variant="primary" icon="plus" onClick={() => setAdding(true)}>
                  Add host
                </Button>
              </EmptyState>
            </Card>
          }
        >
          <Card padding="none">
            <RowList label="Hosts">
              <For each={rows()}>
                {(host) => {
                  const state = () => hostState(host);
                  return (
                    <ListRow
                      data-host={host.name}
                      icon="server"
                      title={host.name}
                      secondary={
                        <span class="nc-mono">
                          {[host.address || host.profile?.address || "no address", labelsText(host.labels)].filter((part) => part !== "").join(" · ")}
                        </span>
                      }
                      status={<State status={state().tone}>{state().label}</State>}
                      meta={
                        <span class="nc-row nc-row--tight">
                          <Badge tone={hostTypeTone(host.type)}>{hostTypeLabel(host.type)}</Badge>
                          <span>{relativeTime(dateOf(host.appliedAt)) || "never applied"}</span>
                        </span>
                      }
                      href={hostPath(host.name)}
                    />
                  );
                }}
              </For>
            </RowList>
          </Card>
        </Show>
      </Show>
      <Show when={adding()}>
        <AddHost
          onClose={() => setAdding(false)}
          onAdded={(name, firstContact) => {
            void hosts.refresh().catch(() => undefined);
            shell.navigate(firstContact === "" ? hostPath(name) : `/jobs/${encodeURIComponent(firstContact)}`);
          }}
        />
      </Show>
    </View>
  );
}

// The home page: what this server holds, and where to go next.
import { For, Show, onSettled } from "solid-js";
import { createQuery } from "@rookery/client/solid";
import { HostType } from "@rookery/proto";
import {
  Badge,
  Callout,
  Card,
  EmptyState,
  ListRow,
  RowList,
  Skeleton,
  StatCard,
} from "@aiviarium/nocturne";

import { useShell } from "../../shell.tsx";
import { Page } from "../../parts/page.tsx";

/** HomePage is the first page a person sees after signing in. */
export function HomePage() {
  const shell = useShell();

  const hosts = createQuery(() => shell.rookery.hosts.list({ limit: 200 }));
  const spaces = createQuery(() => shell.rookery.spaces.list({ limit: 200 }));
  const plugins = createQuery(() => shell.rookery.plugins.list({}));
  const jobs = createQuery(() => shell.rookery.jobs.list({ state: "running", limit: 200 }));

  // The events stream says when one of these changed, so the page follows the
  // server rather than asking again on a timer.
  onSettled(() => {
    const stop = shell.connection.on("rookery/", () => {
      void hosts.refresh().catch(() => undefined);
      void spaces.refresh().catch(() => undefined);
      void plugins.refresh().catch(() => undefined);
      void jobs.refresh().catch(() => undefined);
    });
    return stop;
  });

  const user = () => shell.signIn.user();
  const name = () => user()?.name || user()?.email || "there";

  return (
    <Page title={`Welcome, ${name()}`} description="This is the Rookery server you are signed in to.">
      <div class="shell-stats">
        <StatCard label="Hosts" value={hosts()?.hosts.length ?? "—"} />
        <StatCard label="Spaces" value={spaces()?.spaces.length ?? "—"} />
        <StatCard label="Plugins" value={plugins()?.plugins.length ?? "—"} />
        <StatCard label="Jobs running" value={jobs()?.jobs.length ?? "—"} />
      </div>

      <Show when={hosts.error() !== undefined || spaces.error() !== undefined}>
        <Callout tone="warning">
          Some of this server could not be read. The page shows what it could.
        </Callout>
      </Show>

      <Show when={shell.spaces.currentSpace()}>
        {space => (
          <Card title="The space you are in">
            <div class="nc-row" style={{ "justify-content": "space-between" }}>
              <span class="nc-row nc-row--tight">
                <strong>{space().name}</strong>
                <Badge tone="outline">{space().kind}</Badge>
              </span>
              <span class="nc-meta">Records and documents you open are in this space.</span>
            </div>
          </Card>
        )}
      </Show>

      <Card title="Hosts" count={hosts()?.hosts.length}>
        <Show when={hosts() !== undefined} fallback={<Skeleton width="40%" />}>
          <Show
            when={(hosts()?.hosts.length ?? 0) > 0}
            fallback={
              <EmptyState
                size="sm"
                title="No hosts yet"
                description="A host is a machine Rookery sets up and reaches over SSH and the tailnet."
              >
                <code class="nc-code">rookery host add NAME ADDRESS</code>
              </EmptyState>
            }
          >
            <RowList label="Hosts" variant="inset">
              <For each={hosts()?.hosts ?? []}>
                {host => <ListRow icon="server" title={host.name} secondary={hostType(host.type)} />}
              </For>
            </RowList>
          </Show>
        </Show>
      </Card>

      <Card title="Plugins" count={plugins()?.plugins.length}>
        <Show when={plugins() !== undefined} fallback={<Skeleton width="40%" />}>
          <Show
            when={(plugins()?.plugins.length ?? 0) > 0}
            fallback={<EmptyState size="sm" title="No plugins" description="Rookery ships the Docker and Incus plugins." />}
          >
            <RowList label="Plugins" variant="inset">
              <For each={plugins()?.plugins ?? []}>
                {plugin => (
                  <ListRow
                    icon="puzzle"
                    title={plugin.name || plugin.id}
                    secondary={plugin.version}
                    meta={
                      <Show when={plugin.enabled} fallback={<Badge>disabled</Badge>}>
                        <Badge tone="success">enabled</Badge>
                      </Show>
                    }
                  />
                )}
              </For>
            </RowList>
          </Show>
        </Show>
      </Card>
    </Page>
  );
}

/** hostType is what a host can do, in words. */
function hostType(type: HostType): string {
  switch (type) {
    case HostType.MANAGED:
      return "managed";
    case HostType.WORKSTATION:
      return "workstation";
    default:
      return "ssh";
  }
}

// The home page: what this server holds, and where to go next.
import { For, Show, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import { HostType } from "@rookery/proto";

import { useShell } from "../../shell.tsx";
import { Badge, Callout, Card, Empty, Page, Spinner } from "../../ui.tsx";

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
      <div class="nc-row" style={{ gap: "var(--space-2)", "align-items": "stretch" }}>
        <Stat label="Hosts" value={hosts()?.hosts.length} loading={hosts.pending()} />
        <Stat label="Spaces" value={spaces()?.spaces.length} loading={spaces.pending()} />
        <Stat label="Plugins" value={plugins()?.plugins.length} loading={plugins.pending()} />
        <Stat label="Jobs running" value={jobs()?.jobs.length} loading={jobs.pending()} />
      </div>

      <Show when={hosts.error() !== undefined || spaces.error() !== undefined}>
        <Callout tone="warning">
          <div class="nc-callout__body">
            Some of this server could not be read. The page shows what it could.
          </div>
        </Callout>
      </Show>

      <Show when={shell.spaces.currentSpace()}>
        {space => (
          <Card title="The space you are in">
            <div class="nc-row" style={{ "justify-content": "space-between" }}>
              <span class="nc-row">
                <strong>{space().name}</strong>
                <Badge tone="outline">{space().kind}</Badge>
              </span>
              <span class="nc-meta">Records and documents you open are in this space.</span>
            </div>
          </Card>
        )}
      </Show>

      <Card title="Hosts">
        <Show when={hosts() !== undefined} fallback={<Loading />}>
          <Show
            when={(hosts()?.hosts.length ?? 0) > 0}
            fallback={
              <Empty
                title="No hosts yet"
                description="A host is a machine Rookery sets up and reaches over SSH and the tailnet."
                actions={
                  <span class="nc-meta">
                    Add one with <code class="nc-code">rookery host add NAME ADDRESS</code>.
                  </span>
                }
              />
            }
          >
            <ul class="nc-rows">
              <For each={hosts()?.hosts ?? []}>
                {host => (
                  <li class="nc-lrow" data-interactive>
                    <span class="nc-lrow__main">
                      <span class="nc-lrow-title">{host.name}</span>
                      <span class="nc-lrow-meta">{hostType(host.type)}</span>
                    </span>
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </Show>
      </Card>

      <Card title="Plugins">
        <Show when={plugins() !== undefined} fallback={<Loading />}>
          <Show
            when={(plugins()?.plugins.length ?? 0) > 0}
            fallback={<Empty title="No plugins" description="Rookery ships the Docker and Incus plugins." />}
          >
            <ul class="nc-rows">
              <For each={plugins()?.plugins ?? []}>
                {plugin => (
                  <li class="nc-lrow">
                    <span class="nc-lrow__main">
                      <span class="nc-lrow-title">{plugin.name || plugin.id}</span>
                      <span class="nc-lrow-meta">{plugin.version}</span>
                    </span>
                    <Show when={plugin.enabled} fallback={<Badge>disabled</Badge>}>
                      <Badge tone="success">enabled</Badge>
                    </Show>
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </Show>
      </Card>
    </Page>
  );
}

/** Stat is one number about the server, with what it counts. */
function Stat(props: { label: string; value: number | undefined; loading: boolean }): JSX.Element {
  return (
    <div class="nc-stat" style={{ "min-width": "140px" }}>
      <span class="nc-stat__label">
        {props.label}
        <Show when={props.loading}>
          <Spinner />
        </Show>
      </span>
      <span class="nc-stat__value">{props.value ?? "—"}</span>
    </div>
  );
}

/** Loading is a card whose answer has not arrived. */
function Loading(): JSX.Element {
  return (
    <div class="nc-row">
      <Spinner />
      <span class="nc-meta">Loading…</span>
    </div>
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

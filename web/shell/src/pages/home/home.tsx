// Home: what needs the person, and what this server holds. Unread
// notifications and hosts behind their profile come first, then the jobs that
// ran last and the hosts, each a link to its place.
import { For, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Badge, Callout, Card, EmptyState, ListRow, RowList, Skeleton, State, StatCard } from "@aiviarium/nocturne";

import { createLive } from "../../parts/data.ts";
import { View } from "../../parts/view.tsx";
import { useShell } from "../../shell.tsx";
import { dateOf, hostState, hostTypeLabel, hostTypeTone, relativeTime } from "../infra/format.ts";
import { HOST_EVENTS, hostPath } from "../infra/hosts/paths.ts";
import { JobRows } from "../infra/jobs/rows.tsx";

/** HomePage is the first page a person sees after signing in. */
export function HomePage(): JSX.Element {
  const shell = useShell();
  const hosts = createLive(() => shell.rookery.hosts.list({ limit: 200 }), { events: HOST_EVENTS, space: true });
  const plugins = createLive(() => shell.rookery.plugins.list({}), { events: ["rookery/plugins."] });
  const running = createLive(() => shell.rookery.jobs.list({ state: "running", limit: 200 }), { events: ["rookery/jobs."] });
  const recent = createLive(() => shell.rookery.jobs.list({ limit: 6 }), { events: ["rookery/jobs."] });

  const user = () => shell.signIn.user();
  const name = () => user()?.name || user()?.email || "there";
  const behind = () => (hosts()?.hosts ?? []).filter((host) => hostState(host).tone === "warn");
  const unread = () => shell.notifications.items().filter((item) => !item.read).slice(0, 5);

  return (
    <View id="home" crumbs={[{ label: "Home" }]} reading>
      <div class="shell-greeting">
        <h1 class="title">{`Welcome, ${name()}`}</h1>
        <p class="nc-meta">{shell.spaces.currentSpace() ? `You are in ${shell.spaces.currentSpace()!.name}.` : "You see every space."}</p>
      </div>
      <div class="shell-stats">
        <StatCard label="Hosts" value={hosts()?.hosts.length ?? "—"} />
        <StatCard label="Jobs running" value={running()?.jobs.length ?? "—"} />
        <StatCard label="Plugins" value={plugins()?.plugins.length ?? "—"} />
        <StatCard label="Unread" value={shell.notifications.unread()} />
      </div>
      <Show when={hosts.error() !== undefined}>
        <Callout tone="warning">Some of this server could not be read. The page shows what it could.</Callout>
      </Show>

      <Show when={unread().length > 0 || behind().length > 0}>
        <Card title="Needs you" padding="none" data-part="home-attention">
          <RowList label="Needs you">
            <For each={unread()} keyed={(item) => item.id}>
              {(item) => (
                <ListRow
                  icon="bell"
                  title={item().title}
                  secondary={item().body}
                  unread
                  meta={relativeTime(dateOf(item().createdAt))}
                  href={item().link || "/inbox"}
                />
              )}
            </For>
            <For each={behind()} keyed={(host) => host.name}>
              {(host) => (
                <ListRow
                  icon="server"
                  title={host().name}
                  secondary="Runs an older profile than its own. Apply it to catch up."
                  status={<State status="warn">Behind</State>}
                  href={hostPath(host().name)}
                />
              )}
            </For>
          </RowList>
        </Card>
      </Show>

      <div class="shell-columns">
        <Card title="Recent jobs" padding="none" action={<a class="shell-link nc-meta" href="/jobs">All jobs</a>}>
          <JobRows jobs={recent()?.jobs} label="Recent jobs" resource empty={{ title: "No jobs yet", description: "Applying a host or a roll-out starts one." }} />
        </Card>
        <Card title="Hosts" count={hosts()?.hosts.length} padding="none" action={<a class="shell-link nc-meta" href="/hosts">All hosts</a>}>
          <Show when={hosts() !== undefined} fallback={<div class="shell-pad"><Skeleton width="40%" /></div>}>
            <Show
              when={(hosts()?.hosts.length ?? 0) > 0}
              fallback={
                <EmptyState size="sm" icon="server" title="No hosts yet" description="A host is a machine Rookery sets up and reaches over SSH and the tailnet.">
                  <a class="shell-link" href="/hosts">Add a host</a>
                </EmptyState>
              }
            >
              <RowList label="Hosts">
                <For each={(hosts()?.hosts ?? []).slice(0, 6)} keyed={(host) => host.name}>
                  {(host) => (
                    <ListRow
                      icon="server"
                      title={host().name}
                      status={<State status={hostState(host()).tone}>{hostState(host()).label}</State>}
                      meta={<Badge tone={hostTypeTone(host().type)}>{hostTypeLabel(host().type)}</Badge>}
                      href={hostPath(host().name)}
                    />
                  )}
                </For>
              </RowList>
            </Show>
          </Show>
        </Card>
      </div>
    </View>
  );
}

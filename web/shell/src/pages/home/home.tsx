// Home: what needs the person, what is running, and the places they go. Each
// section shows only while it has something in it; a server with nothing yet
// shows one way to start.
import { For, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import type { Host, Job, Notification, Rollout } from "@rookery/proto";
import { Button, Card, EmptyState, ListRow, RowList, State } from "@aiviarium/nocturne";

import { createLive } from "../../parts/data.ts";
import { View } from "../../parts/view.tsx";
import { useShell } from "../../shell.tsx";
import { spacePath } from "../../spaces.ts";
import { dateOf, hostIssue, hostTypeLabel, jobKindLabel, jobState, relativeTime, resourceLabel, rolloutState } from "../fleet/format.ts";
import { HOST_EVENTS, hostPath } from "../fleet/hosts/paths.ts";
import { jobPath } from "../fleet/jobs/rows.tsx";

/** DAY is how far back a failed job still needs the person. */
const DAY = 86_400_000;

/** HomePage is the first page a person sees after signing in. */
export function HomePage(): JSX.Element {
  const shell = useShell();
  const hosts = createLive(() => shell.rookery.hosts.list({ limit: 500 }), { events: HOST_EVENTS });
  const jobs = createLive(() => shell.rookery.jobs.list({ limit: 50 }), { events: ["rookery/jobs."] });
  const rollouts = createLive(() => shell.rookery.rollouts.list({ state: "", limit: 20 }).catch(() => undefined), {
    events: ["rookery/rollouts."],
  });

  const firstName = () => {
    const user = shell.signIn.user();
    return (user?.name || user?.email.split("@")[0] || "").split(" ")[0] ?? "";
  };
  const greeting = () => {
    const hour = new Date().getHours();
    const part = hour < 12 ? "Good morning" : hour < 18 ? "Good afternoon" : "Good evening";
    return firstName() === "" ? part : `${part}, ${firstName()}`;
  };

  const unread = () => shell.notifications.items().filter((item) => !item.read).slice(0, 5);
  const troubled = () => (hosts()?.hosts ?? []).filter((host) => hostIssue(host)?.tone === "warn");
  const failed = () =>
    (jobs()?.jobs ?? []).filter((job) => {
      const at = dateOf(job.finishedAt ?? job.createdAt)?.getTime() ?? 0;
      return job.state === "failed" && Date.now() - at < DAY;
    });
  const paused = () => (rollouts()?.rollouts ?? []).filter((rollout) => rollout.state === "paused");
  const running = () => (jobs()?.jobs ?? []).filter((job) => job.state === "running" || job.state === "queued");
  const rolling = () => (rollouts()?.rollouts ?? []).filter((rollout) => rollout.state === "running");

  const attention = () => unread().length + troubled().length + failed().length + paused().length;
  const busy = () => running().length + rolling().length;
  const loaded = () => hosts() !== undefined;
  const nothing = () => loaded() && attention() === 0 && busy() === 0 && (hosts()?.hosts.length ?? 0) === 0 && shell.spaces.list().length === 0;

  return (
    <View id="home" crumbs={[{ label: "Home" }]}>
      <div class="shell-home" data-part="home">
        <h1 class="title shell-home__greeting">{greeting()}</h1>
        <Show when={!nothing()} fallback={<FirstSteps />}>
          <Show when={attention() > 0}>
            <Card title="Needs you" count={attention()} padding="none" data-part="home-attention">
              <RowList label="Needs you">
                <For each={unread()} keyed={(item) => item.id}>{(item) => <NotificationRow notification={item()} />}</For>
                <For each={paused()} keyed={(rollout) => rollout.id}>{(rollout) => <RolloutRow rollout={rollout()} />}</For>
                <For each={failed()} keyed={(job) => job.id}>{(job) => <JobRow job={job()} />}</For>
                <For each={troubled()} keyed={(host) => host.name}>{(host) => <HostRow host={host()} />}</For>
              </RowList>
            </Card>
          </Show>
          <Show when={busy() > 0}>
            <Card title="Running" count={busy()} padding="none" data-part="home-running">
              <RowList label="Running">
                <For each={rolling()} keyed={(rollout) => rollout.id}>{(rollout) => <RolloutRow rollout={rollout()} />}</For>
                <For each={running()} keyed={(job) => job.id}>{(job) => <JobRow job={job()} />}</For>
              </RowList>
            </Card>
          </Show>
          <div class="shell-home__places">
            <Show when={shell.spaces.list().length > 0}>
              <Card title="Spaces" padding="none" data-part="home-spaces">
                <RowList label="Spaces">
                  <For each={shell.spaces.list()} keyed={(space) => space.id}>
                    {(space) => (
                      <ListRow
                        icon="layers"
                        title={space().name}
                        secondary={shell.spaces.kinds().find((kind) => kind.id === space().kind)?.label ?? space().kind}
                        href={spacePath(space().id)}
                      />
                    )}
                  </For>
                </RowList>
              </Card>
            </Show>
            <Show when={(hosts()?.hosts.length ?? 0) > 0}>
              <Card title="Hosts" padding="none" action={<a class="shell-link nc-meta" href="/hosts">All hosts</a>} data-part="home-hosts">
                <RowList label="Hosts">
                  <For each={(hosts()?.hosts ?? []).slice(0, 8)} keyed={(host) => host.name}>
                    {(host) => {
                      const issue = () => hostIssue(host());
                      return (
                        <ListRow
                          icon="server"
                          title={host().name}
                          secondary={hostTypeLabel(host().type)}
                          status={issue() === undefined ? undefined : <State status={issue()!.tone}>{issue()!.label}</State>}
                          href={hostPath(host().name)}
                        />
                      );
                    }}
                  </For>
                </RowList>
              </Card>
            </Show>
          </div>
        </Show>
      </div>
    </View>
  );
}

/** FirstSteps is Home on a server with nothing on it yet. */
function FirstSteps(): JSX.Element {
  const shell = useShell();
  return (
    <Card>
      <EmptyState icon="server" title="Add your first host">
        <Button icon="plus" onClick={() => shell.navigate("/hosts?add=1")}>
          Add host
        </Button>
      </EmptyState>
    </Card>
  );
}

function NotificationRow(props: { notification: Notification }): JSX.Element {
  const shell = useShell();
  return (
    <ListRow
      icon="bell"
      title={props.notification.title}
      secondary={props.notification.body}
      unread
      meta={relativeTime(dateOf(props.notification.createdAt))}
      onClick={() => {
        void shell.notifications.markRead(props.notification.id, true).catch(() => undefined);
        shell.navigate(props.notification.link || "/inbox");
      }}
    />
  );
}

function JobRow(props: { job: Job }): JSX.Element {
  const state = () => jobState(props.job.state);
  return (
    <ListRow
      icon="list-checks"
      title={jobKindLabel(props.job.kind)}
      secondary={props.job.resource === "" ? undefined : <span class="nc-mono">{resourceLabel(props.job.resource)}</span>}
      detail={props.job.state === "failed" && props.job.error !== "" ? props.job.error : undefined}
      status={<State status={state().tone}>{state().label}</State>}
      meta={relativeTime(dateOf(props.job.startedAt ?? props.job.createdAt))}
      href={jobPath(props.job.id)}
    />
  );
}

function RolloutRow(props: { rollout: Rollout }): JSX.Element {
  const state = () => rolloutState(props.rollout.state);
  return (
    <ListRow
      icon="git-pull-request"
      title="Roll-out"
      secondary={<span class="nc-mono">{props.rollout.id.slice(0, 8)}</span>}
      status={<State status={state().tone}>{state().label}</State>}
      href={`/rollouts/${encodeURIComponent(props.rollout.id)}`}
    />
  );
}

function HostRow(props: { host: Host }): JSX.Element {
  const issue = () => hostIssue(props.host);
  return (
    <ListRow
      icon="server"
      title={props.host.name}
      status={<State status={issue()?.tone ?? "warn"}>{issue()?.label ?? ""}</State>}
      href={hostPath(props.host.name)}
    />
  );
}

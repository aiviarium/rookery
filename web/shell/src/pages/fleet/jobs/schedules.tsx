// Schedules: a job kind, or a roll-out, on a cron expression in a time zone.
//
// A schedule that is on runs by itself; "Run now" starts it once, at once, and
// the page opens the job or the roll-out it started.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import type { Schedule } from "@rookery/proto";
import { Badge, Button, Card, EmptyState, ListRow, RowList, Select, Skeleton, State, TextField } from "@aiviarium/nocturne";

import { createAction } from "../../../parts/action.ts";
import { createLive } from "../../../parts/data.ts";
import { ConfirmDialog, FormDialog } from "../../../parts/dialogs.tsx";
import { RowActions } from "../../../parts/row-actions.tsx";
import { View } from "../../../parts/view.tsx";
import { useShell } from "../../../shell.tsx";
import { dateOf, errorMessage, jobKindLabel, relativeTime } from "../format.ts";

/** ZONES are the time zones offered first; the browser's own leads. */
function zones(): string[] {
  const own = Intl.DateTimeFormat().resolvedOptions().timeZone;
  return [...new Set(["UTC", own, "Europe/Berlin", "Europe/London", "America/New_York", "America/Los_Angeles", "Asia/Tokyo"])];
}

export function SchedulesPage(): JSX.Element {
  const shell = useShell();
  const schedules = createLive(() => shell.rookery.schedules.list({ limit: 200 }), { events: ["rookery/schedules."] });
  const [creating, setCreating] = createSignal(false);
  const [deleting, setDeleting] = createSignal<Schedule>();
  const rows = (): Schedule[] => schedules()?.schedules ?? [];
  const refresh = () => void schedules.refresh().catch(() => undefined);

  const change = createAction(async (schedule: Schedule, id: string) => {
    if (id === "run") {
      const fired = await shell.rookery.schedules.runNow({ schedule: schedule.id });
      if (fired.rollout !== "") shell.navigate(`/rollouts/${encodeURIComponent(fired.rollout)}`);
      else if (fired.job !== "") shell.navigate(`/jobs/${encodeURIComponent(fired.job)}`);
    }
    if (id === "toggle") await shell.rookery.schedules.update({ schedule: schedule.id, enabled: !schedule.enabled });
    refresh();
  }, { failed: "The schedule did not change" });
  const remove = createAction(async () => {
    const schedule = deleting();
    if (schedule !== undefined) await shell.rookery.schedules.delete({ schedule: schedule.id });
    refresh();
  }, { done: "Schedule deleted" });

  const menu = (schedule: Schedule) => [
    { id: "run", label: "Run now", icon: "play" as const },
    { id: "toggle", label: schedule.enabled ? "Turn off" : "Turn on", icon: schedule.enabled ? ("pause" as const) : ("clock" as const) },
    { type: "separator" as const },
    { id: "delete", label: "Delete", icon: "trash" as const, danger: true },
  ];
  const choose = (schedule: Schedule, id: string) => (id === "delete" ? setDeleting(schedule) : void change.run(schedule, id));

  return (
    <View
      id="schedules"
      crumbs={[{ label: "Schedules" }]}
      error={schedules.error() === undefined ? undefined : `The schedules could not be read. ${errorMessage(schedules.error())}`}
      actions={[
        { id: "new", label: "New schedule", icon: "plus", onClick: () => setCreating(true) },
        { id: "refresh", label: "Refresh", icon: "refresh-cw", overflow: true, onClick: refresh },
      ]}
    >
      <Show when={schedules()} fallback={<Skeleton width="50%" />}>
        <Show
          when={rows().length > 0}
          fallback={
            <Card>
              <EmptyState icon="calendar" title="No schedules">
                <Button icon="plus" onClick={() => setCreating(true)}>
                  New schedule
                </Button>
              </EmptyState>
            </Card>
          }
        >
          <Card padding="none">
            <RowList label="Schedules">
              <For each={rows()} keyed={(schedule) => schedule.id}>
                {(schedule) => (
                  <ListRow
                    data-schedule={schedule().name}
                    icon="calendar"
                    title={schedule().name}
                    secondary={
                      <span class="nc-row nc-row--tight">
                        <Show when={schedule().rolloutCbor.length > 0} fallback={<span>{jobKindLabel(schedule().kind)}</span>}>
                          <Badge tone="outline">roll-out</Badge>
                        </Show>
                        <span class="nc-mono">{`${schedule().cron} · ${schedule().timezone}`}</span>
                      </span>
                    }
                    muted={!schedule().enabled}
                    status={schedule().enabled ? undefined : <State status="off">Off</State>}
                    meta={
                      schedule().enabled && schedule().nextRunAt !== undefined
                        ? `next ${relativeTime(dateOf(schedule().nextRunAt))}`
                        : schedule().lastRunAt !== undefined
                          ? `last ${relativeTime(dateOf(schedule().lastRunAt))}`
                          : undefined
                    }
                    menu={menu(schedule())}
                    onMenuSelect={(id) => choose(schedule(), id)}
                    persistentActions={
                      <RowActions label={`Actions for ${schedule().name}`} items={menu(schedule())} onSelect={(id) => choose(schedule(), id)} />
                    }
                  />
                )}
              </For>
            </RowList>
          </Card>
        </Show>
      </Show>
      <Show when={creating()}>
        <NewSchedule onClose={() => setCreating(false)} onCreated={refresh} />
      </Show>
      <ConfirmDialog
        open={deleting() !== undefined}
        title={`Delete ${deleting()?.name ?? "the schedule"}?`}
        confirm="Delete schedule"
        danger
        action={remove}
        onClose={() => setDeleting(undefined)}
      />
    </View>
  );
}

/** NewSchedule creates a schedule for a job kind. */
function NewSchedule(props: { onClose: () => void; onCreated: () => void }): JSX.Element {
  const shell = useShell();
  const [name, setName] = createSignal("");
  const [kind, setKind] = createSignal("host.apply");
  const [cron, setCron] = createSignal("0 3 * * *");
  const [timezone, setTimezone] = createSignal("UTC");
  const create = createAction(async () => {
    await shell.rookery.schedules.create({ name: name().trim(), kind: kind().trim(), cron: cron().trim(), timezone: timezone() });
  }, { done: "Schedule created" });
  const fields = () => cron().trim().split(/\s+/).length;
  return (
    <FormDialog
      open
      title="New schedule"
      submit="Create schedule"
      action={create}
      disabled={name().trim() === "" || kind().trim() === "" || fields() !== 5}
      onClose={props.onClose}
      onDone={props.onCreated}
    >
      <div class="nc-stack">
        <TextField label="Name" placeholder="Nightly apply" value={name()} onInput={(event) => setName(event.currentTarget.value)} />
        <TextField
          label="Job kind"
          mono
          info="A core kind such as host.apply, or a plugin's. A roll-out on a schedule is made by its plugin or with rookery rollout."
          value={kind()}
          onInput={(event) => setKind(event.currentTarget.value)}
        />
        <div class="shell-form__row">
          <TextField
            label="When"
            mono
            info="Five fields: minute, hour, day, month, weekday."
            error={cron().trim() !== "" && fields() !== 5 ? "Five fields." : undefined}
            value={cron()}
            onInput={(event) => setCron(event.currentTarget.value)}
          />
          <Select label="Time zone" value={timezone()} options={zones().map((zone) => ({ value: zone, label: zone }))} onChange={setTimezone} />
        </div>
      </div>
    </FormDialog>
  );
}

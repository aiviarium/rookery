// Schedules: a job kind, or a roll-out, on a cron expression in a time zone.
//
// A schedule that is enabled runs by itself; "Run now" starts it once, at once,
// and the page opens the job or the roll-out it started.
import { Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import type { Schedule } from "@rookery/proto";

import { useShell } from "../../../shell.tsx";
import { Badge, Button, Callout, Card, Field, IconButton, Input, Page, Select, Spinner } from "../../../ui.tsx";
import { errorMessage, relativeTime, dateOf } from "../format.ts";
import { Dialog, StateText, Table, type Column } from "../parts.tsx";

export function SchedulesPage(): JSX.Element {
  const shell = useShell();
  const schedules = createQuery(() => shell.rookery.schedules.list({ limit: 200 }));
  const [creating, setCreating] = createSignal(false);
  const [failure, setFailure] = createSignal<string>();

  const rows = (): Schedule[] => schedules()?.schedules ?? [];

  const run = async (schedule: Schedule): Promise<void> => {
    setFailure(undefined);
    try {
      const fired = await shell.rookery.schedules.runNow({ schedule: schedule.id });
      if (fired.rollout !== "") shell.navigate(`/rollouts/${encodeURIComponent(fired.rollout)}`);
      else if (fired.job !== "") shell.navigate(`/jobs/${encodeURIComponent(fired.job)}`);
      void schedules.refresh().catch(() => undefined);
    } catch (error) {
      setFailure(errorMessage(error));
    }
  };

  const enable = async (schedule: Schedule, enabled: boolean): Promise<void> => {
    setFailure(undefined);
    try {
      await shell.rookery.schedules.update({ schedule: schedule.id, enabled });
      void schedules.refresh().catch(() => undefined);
    } catch (error) {
      setFailure(errorMessage(error));
    }
  };

  const remove = async (schedule: Schedule): Promise<void> => {
    setFailure(undefined);
    try {
      await shell.rookery.schedules.delete({ schedule: schedule.id });
      void schedules.refresh().catch(() => undefined);
    } catch (error) {
      setFailure(errorMessage(error));
    }
  };

  const columns: Column<Schedule>[] = [
    { id: "name", header: "Name", value: schedule => schedule.name },
    {
      id: "runs",
      header: "Runs",
      value: schedule => (schedule.rolloutCbor.length > 0 ? "roll-out" : schedule.kind),
      cell: schedule =>
        schedule.rolloutCbor.length > 0 ? <Badge tone="outline">roll-out</Badge> : <span class="nc-code">{schedule.kind}</span>,
    },
    { id: "cron", header: "Cron", mono: true, value: schedule => `${schedule.cron} ${schedule.timezone}` },
    {
      id: "enabled",
      header: "On",
      value: schedule => (schedule.enabled ? "on" : "off"),
      cell: schedule => <StateText tone={schedule.enabled ? "ok" : "off"}>{schedule.enabled ? "On" : "Off"}</StateText>,
    },
    { id: "next", header: "Next", value: schedule => relativeTime(dateOf(schedule.nextRunAt)) },
    { id: "last", header: "Last", value: schedule => relativeTime(dateOf(schedule.lastRunAt)) },
  ];

  return (
    <Page
      title="Schedules"
      description="A job kind or a roll-out on a cron expression, in a time zone. A missed run is enqueued once."
      actions={
        <>
          <IconButton label="Read the schedules again" glyph="jobs" onClick={() => void schedules.refresh()} />
          <Button variant="primary" onClick={() => setCreating(true)}>
            New schedule
          </Button>
        </>
      }
    >
      <Show when={schedules.error()}>{error => <Callout tone="danger">{error().message}</Callout>}</Show>
      <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
      <Card title="Schedules">
        <p class="nc-meta">{rows().length} {rows().length === 1 ? "schedule" : "schedules"}</p>
        <Show when={!schedules.pending() || schedules() !== undefined} fallback={<Spinner />}>
          <Table
            label="Schedules"
            rows={rows()}
            columns={columns}
            rowKey={schedule => schedule.id}
            empty="No schedules. A schedule runs a job kind or a roll-out by itself."
            rowActions={schedule => (
              <span class="nc-row">
                <Button size="sm" onClick={() => void run(schedule)}>
                  Run now
                </Button>
                <Button size="sm" onClick={() => void enable(schedule, !schedule.enabled)}>
                  {schedule.enabled ? "Disable" : "Enable"}
                </Button>
                <Button size="sm" variant="danger" onClick={() => void remove(schedule)}>
                  Delete
                </Button>
              </span>
            )}
          />
        </Show>
      </Card>
      <Show when={creating()}>
        <NewSchedule
          onClose={() => setCreating(false)}
          onCreated={() => {
            setCreating(false);
            void schedules.refresh().catch(() => undefined);
          }}
        />
      </Show>
    </Page>
  );
}

/** NewSchedule creates a schedule for a job kind. */
function NewSchedule(props: { onClose: () => void; onCreated: () => void }): JSX.Element {
  const shell = useShell();
  const [name, setName] = createSignal("");
  const [kind, setKind] = createSignal("host.apply");
  const [cron, setCron] = createSignal("0 3 * * *");
  const [timezone, setTimezone] = createSignal("UTC");
  const [busy, setBusy] = createSignal(false);
  const [failure, setFailure] = createSignal<string>();

  const submit = async (): Promise<void> => {
    if (busy()) return;
    setBusy(true);
    setFailure(undefined);
    try {
      await shell.rookery.schedules.create({
        name: name().trim(),
        kind: kind().trim(),
        cron: cron().trim(),
        timezone: timezone().trim(),
      });
      props.onCreated();
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open
      onClose={props.onClose}
      title="New schedule"
      description="A five-field cron expression in an IANA time zone."
      footer={
        <>
          <Button onClick={props.onClose} disabled={busy()}>
            Cancel
          </Button>
          <Button
            variant="primary"
            disabled={busy() || name().trim() === "" || kind().trim() === "" || cron().trim() === ""}
            onClick={() => void submit()}
          >
            {busy() ? "Creating…" : "Create schedule"}
          </Button>
        </>
      }
    >
      <Field label="Name">
        <Input value={name()} placeholder="Nightly apply" onInput={setName} />
      </Field>
      <Field label="Job kind" hint="A core kind such as host.apply, or a plugin's.">
        <Input value={kind()} onInput={setKind} />
      </Field>
      <div class="nc-row">
        <Field label="Cron">
          <Input value={cron()} onInput={setCron} />
        </Field>
        <Field label="Time zone">
          <Select
            value={timezone()}
            options={[
              { value: "UTC", label: "UTC" },
              { value: "Europe/Berlin", label: "Europe/Berlin" },
              { value: "America/New_York", label: "America/New_York" },
              { value: "Asia/Tokyo", label: "Asia/Tokyo" },
            ]}
            onChange={setTimezone}
          />
        </Field>
      </div>
      <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
      <p class="nc-muted">
        A schedule that starts a roll-out is made by its plugin or by <span class="nc-code">rookery rollout</span>.
      </p>
    </Dialog>
  );
}

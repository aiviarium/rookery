// Schedules: a job kind, or a roll-out, on a cron expression in a time zone.
//
// A schedule that is enabled runs by itself; "Run now" starts it once, at once,
// and the page opens the job or the roll-out it started.
import { Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import type { Schedule } from "@rookery/proto";
import {
  Badge,
  Button,
  Callout,
  Card,
  DataTable,
  type DataColumn,
  Dialog,
  Select,
  State,
  TextField,
} from "@aiviarium/nocturne";

import { useShell } from "../../../shell.tsx";
import { Page } from "../../../parts/page.tsx";
import { errorMessage, relativeTime, dateOf } from "../format.ts";

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

  const columns = (): DataColumn<Schedule>[] => [
    { id: "name", header: "Name", accessor: schedule => schedule.name },
    {
      id: "runs",
      header: "Runs",
      accessor: schedule => (schedule.rolloutCbor.length > 0 ? "roll-out" : schedule.kind),
      cell: schedule =>
        schedule.rolloutCbor.length > 0 ? <Badge tone="outline">roll-out</Badge> : <span class="nc-code">{schedule.kind}</span>,
    },
    { id: "cron", header: "Cron", type: "mono", accessor: schedule => `${schedule.cron} ${schedule.timezone}` },
    {
      id: "enabled",
      header: "On",
      accessor: schedule => (schedule.enabled ? "on" : "off"),
      cell: schedule => <State status={schedule.enabled ? "ok" : "off"}>{schedule.enabled ? "On" : "Off"}</State>,
    },
    { id: "next", header: "Next", type: "time", accessor: schedule => relativeTime(dateOf(schedule.nextRunAt)) },
    { id: "last", header: "Last", type: "time", accessor: schedule => relativeTime(dateOf(schedule.lastRunAt)) },
    {
      id: "actions",
      header: "",
      cell: schedule => (
        <span class="nc-row nc-row--tight">
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
      ),
    },
  ];

  return (
    <Page
      title="Schedules"
      description="A job kind or a roll-out on a cron expression, in a time zone. A missed run is enqueued once."
      actions={() => [
        { id: "refresh", label: "Read the schedules again", icon: "refresh-cw", onClick: () => void schedules.refresh() },
        { id: "new", label: "New schedule", primary: true, onClick: () => setCreating(true) },
      ]}
    >
      <Show when={schedules.error()}>{error => <Callout tone="danger">{error().message}</Callout>}</Show>
      <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
      <Card title="Schedules" count={rows().length} padding="none">
        <DataTable
          bare
          label="Schedules"
          rows={rows()}
          columns={columns()}
          getRowId={schedule => schedule.id}
          loading={schedules.pending() && schedules() === undefined}
          empty={{
            title: "No schedules",
            description: "A schedule runs a job kind or a roll-out by itself.",
          }}
        />
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
            loading={busy()}
            disabled={name().trim() === "" || kind().trim() === "" || cron().trim() === ""}
            onClick={() => void submit()}
          >
            Create schedule
          </Button>
        </>
      }
    >
      <div class="nc-stack">
        <TextField
          label="Name"
          value={name()}
          placeholder="Nightly apply"
          onInput={event => setName(event.currentTarget.value)}
        />
        <TextField
          label="Job kind"
          help="A core kind such as host.apply, or a plugin's."
          value={kind()}
          onInput={event => setKind(event.currentTarget.value)}
        />
        <div class="nc-row">
          <TextField label="Cron" value={cron()} onInput={event => setCron(event.currentTarget.value)} />
          <Select
            label="Time zone"
            value={timezone()}
            options={[
              { value: "UTC", label: "UTC" },
              { value: "Europe/Berlin", label: "Europe/Berlin" },
              { value: "America/New_York", label: "America/New_York" },
              { value: "Asia/Tokyo", label: "Asia/Tokyo" },
            ]}
            onChange={setTimezone}
          />
        </div>
        <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
        <p class="nc-muted">
          A schedule that starts a roll-out is made by its plugin or by <span class="nc-code">rookery rollout</span>.
        </p>
      </div>
    </Dialog>
  );
}

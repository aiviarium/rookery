// Jobs: everything Rookery runs, newest first, as it changes. A person filters
// by state and kind; a job's log, cancel and retry are on its own page.
import { createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useSearchParams } from "@solidjs/router";
import { Card, FilterBar, Select, TextField } from "@aiviarium/nocturne";

import { createLive } from "../../../parts/data.ts";
import { errorMessage } from "../../../parts/errors.ts";
import { View } from "../../../parts/view.tsx";
import { useShell } from "../../../shell.tsx";
import { jobState } from "../format.ts";
import { JobRows } from "./rows.tsx";

export { jobPath } from "./rows.tsx";

const STATES = ["", "queued", "running", "succeeded", "failed", "cancelled", "interrupted"] as const;

/** JobsPage lists the newest jobs, filtered by the state and kind in the address. */
export function JobsPage(): JSX.Element {
  const shell = useShell();
  const [search, setSearch] = useSearchParams<{ state?: string; kind?: string }>();
  const [kind, setKind] = createSignal(search.kind ?? "");
  const state = () => search.state ?? "";
  const jobs = createLive(() => shell.rookery.jobs.list({ kind: (search.kind ?? "").trim(), state: state(), limit: 200 }), {
    key: () => `${search.kind ?? ""}/${state()}`,
    events: ["rookery/jobs."],
  });
  const filtered = () => state() !== "" || (search.kind ?? "") !== "";

  return (
    <View
      id="jobs"
      crumbs={[{ label: "Jobs" }]}
      error={jobs.error() === undefined ? undefined : `The jobs could not be read. ${errorMessage(jobs.error())}`}
      actions={[
        ...(filtered()
          ? [{ id: "clear", label: "Clear filters", icon: "close" as const, onClick: () => (setKind(""), setSearch({ state: undefined, kind: undefined })) }]
          : []),
      ]}
    >
      <FilterBar
        label="Job filters"
        search={
          <form
            onSubmit={(event) => {
              event.preventDefault();
              setSearch({ kind: kind().trim() === "" ? undefined : kind().trim() });
            }}
          >
            <TextField
              variant="search"
              aria-label="Kind"
              placeholder="Kind, such as host.apply"
              value={kind()}
              onInput={(event) => setKind(event.currentTarget.value)}
              onBlur={() => setSearch({ kind: kind().trim() === "" ? undefined : kind().trim() })}
            />
          </form>
        }
      >
        <Select
          aria-label="State"
          value={state()}
          options={STATES.map((value) => ({ value, label: value === "" ? "Every state" : jobState(value).label }))}
          onChange={(next) => setSearch({ state: next === "" ? undefined : next })}
        />
      </FilterBar>
      <Card padding="none">
        <JobRows
          jobs={jobs()?.jobs}
          label="Jobs"
          resource
          empty={{ title: filtered() ? "No job matches" : "No jobs" }}
        />
      </Card>
    </View>
  );
}

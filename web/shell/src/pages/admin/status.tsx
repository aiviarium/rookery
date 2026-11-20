// The server's own status: whether it leads, what it holds, and what changed
// lately.
import { For, Show, createSignal, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import { timestampDate } from "@bufbuild/protobuf/wkt";
import type { Timestamp } from "@bufbuild/protobuf/wkt";

import { useShell } from "../../shell.tsx";
import { Badge, Button, Card, Empty, Page, Spinner, Table } from "../../ui.tsx";

/** ServerStatusPage is the deployment's state, not the hosts'. */
export function ServerStatusPage(): JSX.Element {
  const shell = useShell();
  const hosts = createQuery(() => shell.rookery.hosts.list({ limit: 200 }));
  const jobs = createQuery(() => shell.rookery.jobs.list({ limit: 50 }));
  const plugins = createQuery(() => shell.rookery.plugins.list({}));
  const spaces = createQuery(() => shell.rookery.spaces.list({ includeArchived: true, limit: 200 }));
  const tailnet = createQuery(() => shell.rookery.tailnet.status({}));
  const audit = createQuery(() => shell.rookery.audit.list({ limit: 20 }));

  const [health, setHealth] = createSignal("checking");

  const check = async () => {
    setHealth("checking");
    try {
      const response = await fetch(shell.rookery.url("/healthz"), { credentials: "include" });
      setHealth(response.ok ? "leader" : response.status === 503 ? "standby" : `http ${response.status}`);
    } catch {
      setHealth("unreachable");
    }
  };
  void check();

  onSettled(() => {
    const stop = shell.connection.on("rookery/", () => {
      void jobs.refresh().catch(() => undefined);
      void audit.refresh().catch(() => undefined);
    });
    return stop;
  });

  return (
    <Page
      title="Server"
      description="This server's state: whether it leads, what it holds, and what changed."
      actions={
        <Button
          size="sm"
          variant="ghost"
          onClick={() => {
            void check();
          }}
        >
          Check again
        </Button>
      }
    >
      <div class="nc-row" style={{ gap: "var(--space-2)" }}>
        <Stat label="Leadership">
          <Badge tone={health() === "leader" ? "success" : health() === "standby" ? "warning" : "danger"}>{health()}</Badge>
        </Stat>
        <Stat label="Hosts" value={hosts()?.hosts.length} />
        <Stat label="Spaces" value={spaces()?.spaces.length} />
        <Stat label="Plugins" value={plugins()?.plugins.length} />
        <Stat label="Running jobs" value={jobs()?.jobs.filter((job) => job.state === "running").length} />
      </div>

      <Card title="Tailnet">
        <Show when={tailnet() !== undefined} fallback={<Spinner />}>
          <div class="nc-row" style={{ gap: "var(--space-3)" }}>
            <Badge tone={tailnet()?.configured ? "success" : "outline"}>
              {tailnet()?.configured ? "configured" : "not configured"}
            </Badge>
            <Show when={tailnet()?.configured}>
              <Badge tone={tailnet()?.online ? "success" : "warning"}>{tailnet()?.online ? "online" : "offline"}</Badge>
              <span class="nc-meta">{tailnet()?.controlUrl}</span>
            </Show>
          </div>
        </Show>
      </Card>

      <Card title="Recent jobs">
        <Show when={jobs() !== undefined} fallback={<Spinner />}>
          <Show
            when={(jobs()?.jobs.length ?? 0) > 0}
            fallback={<Empty title="No jobs yet" description="An apply, a host action or a roll-out is a job with a log." />}
          >
            <Table columns={["Kind", "State", "Resource", "Started"]}>
              <For each={jobs()?.jobs ?? []}>
                {job => (
                  <tr>
                    <td>{job.kind}</td>
                    <td>
                      <Badge tone={job.state === "failed" ? "danger" : job.state === "running" ? "info" : "outline"}>{job.state}</Badge>
                    </td>
                    <td class="nc-mono">{job.resource}</td>
                    <td>{when(job.createdAt)}</td>
                  </tr>
                )}
              </For>
            </Table>
          </Show>
        </Show>
      </Card>

      <Card title="Audit">
        <Show when={audit() !== undefined} fallback={<Spinner />}>
          <Show
            when={(audit()?.entries.length ?? 0) > 0}
            fallback={<Empty title="Nothing audited yet" description="Every call that changes state is recorded here." />}
          >
            <Table columns={["When", "Who", "Call", "Outcome"]}>
              <For each={audit()?.entries ?? []}>
                {entry => (
                  <tr>
                    <td>{when(entry.at)}</td>
                    <td class="nc-mono">{entry.principal}</td>
                    <td class="nc-mono">{entry.procedure}</td>
                    <td>
                      <Badge tone={entry.outcome === "ok" ? "success" : entry.outcome === "denied" ? "warning" : "danger"}>
                        {entry.outcome}
                      </Badge>
                    </td>
                  </tr>
                )}
              </For>
            </Table>
          </Show>
        </Show>
      </Card>
    </Page>
  );
}

/** Stat is one number about the server. */
function Stat(props: { label: string; value?: number; children?: JSX.Element }): JSX.Element {
  return (
    <div class="nc-stat" style={{ "min-width": "120px" }}>
      <span class="nc-stat__label">{props.label}</span>
      <span class="nc-stat__value">{props.children ?? props.value ?? "—"}</span>
    </div>
  );
}

/** when is a timestamp as the reader's clock shows it. */
function when(at: Timestamp | undefined): string {
  return at === undefined ? "" : timestampDate(at).toLocaleString();
}

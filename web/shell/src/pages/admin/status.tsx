// The server's own status: whether it leads, what it holds, and what changed
// lately.
import { createSignal, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";
import { timestampDate } from "@bufbuild/protobuf/wkt";
import type { Timestamp } from "@bufbuild/protobuf/wkt";
import type { AuditEntry, Job } from "@rookery/proto";
import { Badge, Card, DataTable, type DataColumn, MetaLine, StatCard } from "@aiviarium/nocturne";

import { useShell } from "../../shell.tsx";
import { Page } from "../../parts/page.tsx";

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

  const jobColumns = (): DataColumn<Job>[] => [
    { id: "kind", header: "Kind", accessor: job => job.kind },
    {
      id: "state",
      header: "State",
      accessor: job => job.state,
      cell: job => (
        <Badge tone={job.state === "failed" ? "danger" : job.state === "running" ? "info" : "outline"}>{job.state}</Badge>
      ),
    },
    { id: "resource", header: "Resource", type: "mono", accessor: job => job.resource },
    { id: "started", header: "Started", type: "time", accessor: job => when(job.createdAt) },
  ];

  const auditColumns = (): DataColumn<AuditEntry>[] => [
    { id: "when", header: "When", type: "time", accessor: entry => when(entry.at) },
    { id: "who", header: "Who", type: "mono", accessor: entry => entry.principal },
    { id: "call", header: "Call", type: "mono", accessor: entry => entry.procedure },
    {
      id: "outcome",
      header: "Outcome",
      accessor: entry => entry.outcome,
      cell: entry => (
        <Badge tone={entry.outcome === "ok" ? "success" : entry.outcome === "denied" ? "warning" : "danger"}>
          {entry.outcome}
        </Badge>
      ),
    },
  ];

  return (
    <Page
      title="Server"
      description="This server's state: whether it leads, what it holds, and what changed."
      actions={() => [
        { id: "check", label: "Check again", icon: "refresh-cw", onClick: () => void check() },
      ]}
    >
      <div class="shell-stats">
        <StatCard label="Leadership" value={health()} />
        <StatCard label="Hosts" value={hosts()?.hosts.length ?? "—"} />
        <StatCard label="Spaces" value={spaces()?.spaces.length ?? "—"} />
        <StatCard label="Plugins" value={plugins()?.plugins.length ?? "—"} />
        <StatCard label="Running jobs" value={jobs()?.jobs.filter((job) => job.state === "running").length ?? "—"} />
      </div>

      <Card title="Tailnet">
        <MetaLine
          items={[
            {
              label: (
                <Badge tone={tailnet()?.configured ? "success" : "outline"}>
                  {tailnet()?.configured ? "configured" : "not configured"}
                </Badge>
              ),
            },
            ...(tailnet()?.configured
              ? [
                  {
                    label: (
                      <Badge tone={tailnet()?.online ? "success" : "warning"}>
                        {tailnet()?.online ? "online" : "offline"}
                      </Badge>
                    ),
                  },
                  { label: tailnet()?.controlUrl ?? "", mono: true },
                ]
              : []),
          ]}
        />
      </Card>

      <Card title="Recent jobs" padding="none">
        <DataTable
          bare
          label="Recent jobs"
          rows={jobs()?.jobs ?? []}
          columns={jobColumns()}
          getRowId={job => job.id}
          loading={jobs.pending() && jobs() === undefined}
          empty={{ title: "No jobs yet", description: "An apply, a host action or a roll-out is a job with a log." }}
        />
      </Card>

      <Card title="Audit" padding="none">
        <DataTable
          bare
          label="The audit trail"
          rows={audit()?.entries ?? []}
          columns={auditColumns()}
          getRowId={entry => String(entry.id)}
          loading={audit.pending() && audit() === undefined}
          empty={{ title: "Nothing audited yet", description: "Every call that changes state is recorded here." }}
        />
      </Card>
    </Page>
  );
}

/** when is a timestamp as the reader's clock shows it. */
function when(at: Timestamp | undefined): string {
  return at === undefined ? "" : timestampDate(at).toLocaleString();
}

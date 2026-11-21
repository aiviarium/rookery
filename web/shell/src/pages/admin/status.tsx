// The server itself: whether this instance leads, how it reaches its hosts, and
// the audit trail of every call that changed state. The hosts, jobs and plugins
// have places of their own; Home sums them up.
import { Show, createSignal, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";
import type { AuditEntry } from "@rookery/proto";
import { Badge, Card, DataTable, type DataColumn, PropertyList, State, type DotStatus } from "@aiviarium/nocturne";

import { createLive } from "../../parts/data.ts";
import { View } from "../../parts/view.tsx";
import { ADMIN } from "./people.tsx";
import { useShell } from "../../shell.tsx";
import { dateOf, relativeTime } from "../fleet/format.ts";

type Health = "checking" | "leader" | "standby" | "unreachable" | string;

/** ServerStatusPage is the deployment's state, not the hosts'. */
export function ServerStatusPage(): JSX.Element {
  const shell = useShell();
  const tailnet = createLive(() => shell.rookery.tailnet.status({}));
  const audit = createLive(() => shell.rookery.audit.list({ limit: 100 }), { events: ["rookery/"] });
  const users = createLive(() => shell.rookery.users.list({ limit: 500 }));
  const who = (principal: string): string => {
    const id = principal.startsWith("user:") ? principal.slice(5) : "";
    return (id === "" ? undefined : users()?.users.find((user) => user.id === id)?.email) ?? principal;
  };
  const [health, setHealth] = createSignal<Health>("checking");

  const check = async () => {
    setHealth("checking");
    try {
      const response = await fetch(shell.rookery.url("/healthz"), { credentials: "include" });
      setHealth(response.ok ? "leader" : response.status === 503 ? "standby" : `HTTP ${response.status}`);
    } catch {
      setHealth("unreachable");
    }
  };
  onSettled(() => void check());

  const healthDot = (): DotStatus => (health() === "leader" ? "ok" : health() === "standby" ? "idle" : health() === "checking" ? "off" : "error");
  const healthLabel = (): string =>
    health() === "leader"
      ? "Leads"
      : health() === "standby"
        ? "Standby"
        : health() === "checking"
          ? "Checking"
          : health() === "unreachable"
            ? "Unreachable"
            : health();

  const columns = (): DataColumn<AuditEntry>[] => [
    { id: "when", header: "When", accessor: (entry) => relativeTime(dateOf(entry.at)) },
    { id: "who", header: "Who", accessor: (entry) => who(entry.principal) },
    { id: "call", header: "Call", type: "mono", accessor: (entry) => entry.procedure.replace(/^\/rookery\.v1\./, "") },
    {
      id: "outcome",
      header: "Outcome",
      accessor: (entry) => entry.outcome,
      cell: (entry) => (
        <Badge tone={entry.outcome === "ok" ? "success" : entry.outcome === "denied" ? "warning" : "danger"}>{entry.outcome}</Badge>
      ),
    },
  ];

  return (
    <View
      id="server"
      crumbs={[ADMIN, { label: "Server" }]}
      context={<State status={healthDot()}>{healthLabel()}</State>}
      actions={[
        {
          id: "refresh",
          label: "Check again",
          icon: "refresh-cw",
          overflow: true,
          onClick: () => {
            void check();
            void tailnet.refresh().catch(() => undefined);
            void audit.refresh().catch(() => undefined);
          },
        },
      ]}
    >
      <Card title="This instance">
        <PropertyList
          items={[
            { label: "Role", value: <State status={healthDot()}>{healthLabel()}</State> },
            { label: "Address", value: <span class="nc-mono">{shell.rookery.url("/")}</span> },
            {
              label: "Tailnet",
              value: (
                <Show when={tailnet()} fallback="…">
                  {(now) => (
                    <State status={!now().configured ? "off" : now().online ? "ok" : "warn"}>
                      {!now().configured ? "Not configured" : now().online ? `Online at ${now().addresses[0] ?? "no address"}` : "Offline"}
                    </State>
                  )}
                </Show>
              ),
            },
          ]}
        />
      </Card>
      <Card title="Audit" padding="none">
        <DataTable
          bare
          label="Audit trail"
          rows={audit()?.entries ?? []}
          columns={columns()}
          getRowId={(entry) => String(entry.id)}
          loading={audit() === undefined}
          error={audit.error() === undefined ? undefined : { message: "The audit trail could not be read.", detail: audit.error()?.message }}
          empty={{ title: "Nothing audited" }}
        />
      </Card>
    </View>
  );
}

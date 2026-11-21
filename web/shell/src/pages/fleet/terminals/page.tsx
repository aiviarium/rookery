// Terminals: every shell running on any host. A terminal lives on its host and
// outlives the page, so this is the list, and each one opens in its host's
// Terminal place, where the tabs and the screen are.
import { For, Show, createSignal, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import type { Host, Terminal } from "@rookery/proto";
import { Button, Card, EmptyState, ListRow, RowList, Select, Skeleton, State } from "@aiviarium/nocturne";

import { createAction } from "../../../parts/action.ts";
import { createLive } from "../../../parts/data.ts";
import { FormDialog } from "../../../parts/dialogs.tsx";
import { errorMessage } from "../../../parts/errors.ts";
import { View } from "../../../parts/view.tsx";
import { useShell } from "../../../shell.tsx";
import { terminalTitle, type Account } from "../hosts/terminal.tsx";
import { HOST_EVENTS } from "../hosts/paths.ts";
import { HostType } from "@rookery/proto";

/** terminalPath opens a terminal in its host's Terminal place. */
export function terminalPath(host: string, id: string): string {
  return `/hosts/${encodeURIComponent(host)}/terminal?attach=${encodeURIComponent(id)}`;
}

export function TerminalsPage(): JSX.Element {
  const shell = useShell();
  const hosts = createLive(() => shell.rookery.hosts.list({ limit: 500 }), { events: HOST_EVENTS });
  const terminals = createLive(async () => {
    const listed = await shell.rookery.hosts.list({ limit: 500 });
    const managed = listed.hosts.filter((host) => host.type !== HostType.SSH);
    const all = await Promise.all(
      managed.map((host) => shell.rookery.terminals.list({ host: host.name }).then((answer) => answer.terminals).catch(() => [] as Terminal[])),
    );
    return all.flat().filter((terminal) => terminal.session !== undefined);
  });
  const [opening, setOpening] = createSignal(false);
  const rows = () => terminals() ?? [];

  return (
    <View
      id="terminals"
      crumbs={[{ label: "Terminals" }]}
      error={terminals.error() === undefined ? undefined : `The terminals could not be read. ${errorMessage(terminals.error())}`}
      actions={[
        { id: "open", label: "New terminal", icon: "plus", onClick: () => setOpening(true) },
        { id: "refresh", label: "Refresh", icon: "refresh-cw", overflow: true, onClick: () => void terminals.refresh().catch(() => undefined) },
      ]}
    >
      <Show when={terminals()} fallback={<Skeleton width="50%" />}>
        <Show
          when={rows().length > 0}
          fallback={
            <Card>
              <EmptyState icon="terminal" title="No terminals">
                <Button icon="plus" onClick={() => setOpening(true)}>
                  New terminal
                </Button>
              </EmptyState>
            </Card>
          }
        >
          <Card padding="none">
            <RowList label="Terminals">
              <For each={rows()} keyed={(terminal) => terminal.session?.id ?? ""}>
                {(terminal) => (
                  <ListRow
                    data-terminal={terminal().session?.id}
                    icon="terminal"
                    title={terminal().host}
                    secondary={terminalTitle(terminal())}
                    status={
                      <State status={terminal().session?.running ? "live" : "off"}>
                        {terminal().session?.running ? "Running" : `Exited ${terminal().session?.exitCode ?? 0}`}
                      </State>
                    }
                    href={terminalPath(terminal().host, terminal().session?.id ?? "")}
                  />
                )}
              </For>
            </RowList>
          </Card>
        </Show>
      </Show>
      <Show when={opening()}>
        <NewTerminal hosts={(hosts()?.hosts ?? []).filter((host) => host.type !== HostType.SSH)} onClose={() => setOpening(false)} />
      </Show>
    </View>
  );
}

function NewTerminal(props: { hosts: readonly Host[]; onClose: () => void }): JSX.Element {
  const shell = useShell();
  // The dialog starts on the first host the server lists.
  const [host, setHost] = createSignal(untrack(() => props.hosts[0]?.name ?? ""));
  const [account, setAccount] = createSignal<Account>("operator");
  const workstation = () => props.hosts.find((one) => one.name === host())?.type === HostType.WORKSTATION;
  const open = createAction(async () => {
    const opened = await shell.rookery.terminals.open({ host: host(), session: { user: { name: account() }, cols: 120, rows: 32 } });
    const id = opened.terminal?.session?.id ?? "";
    if (id === "") throw new Error("The host opened no terminal.");
    shell.navigate(terminalPath(host(), id));
  });
  return (
    <FormDialog
      open
      title="New terminal"
      submit="Open"
      action={open}
      disabled={host() === ""}
      onClose={props.onClose}
    >
      <div class="nc-stack">
        <Select
          label="Host"
          placeholder={props.hosts.length === 0 ? "No managed host yet" : "Choose a host"}
          value={host()}
          options={props.hosts.map((one) => ({ value: one.name, label: one.name }))}
          onChange={(next) => {
            setHost(next);
            if (account() === "desktop" && props.hosts.find((one) => one.name === next)?.type !== HostType.WORKSTATION) setAccount("operator");
          }}
        />
        <Select
          label="As"
          value={account()}
          options={[
            { value: "operator", label: "Operator", description: "Needs manage on the host." },
            ...(workstation() ? [{ value: "desktop", label: "The desktop's account", description: "Needs use on the host." }] : []),
            { value: "root", label: "Root", description: "Needs manage on the host." },
          ]}
          onChange={(next) => setAccount(next as Account)}
        />
      </div>
    </FormDialog>
  );
}

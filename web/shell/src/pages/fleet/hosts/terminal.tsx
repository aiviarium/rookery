// A host's Terminal place: one of its terminals, or the list of them.
//
// A terminal lives on its host and outlives the page. The address names the
// one a tab shows (`?attach=`), so a terminal is a tab of the shell's
// workbench like any page: several sit side by side or stacked by splitting
// the workbench, and a reload brings each back. Without one named, the place
// lists the host's running terminals. Closing a tab leaves its shell running;
// ending a terminal is the page's own action.
import { For, Show, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useSearchParams } from "@solidjs/router";
import type { Terminal } from "@rookery/proto";
import {
  Button,
  Card,
  EmptyState,
  ListRow,
  RowList,
  Skeleton,
  State,
  type ViewAction,
} from "@aiviarium/nocturne";

import { usePageTab } from "../../../chrome/workbench-view.tsx";
import { createAction } from "../../../parts/action.ts";
import { createLive } from "../../../parts/data.ts";
import { errorMessage } from "../../../parts/errors.ts";
import { View } from "../../../parts/view.tsx";
import { type TabPlace, useShell } from "../../../shell.tsx";
import { TerminalPane } from "../terminals/pane.tsx";
import { useHost } from "./domain.tsx";
import { hostPath, terminalPath } from "./paths.ts";

/** Account is who a new shell runs as on the host. */
export type Account = "operator" | "desktop" | "root";

const ACCOUNT_LABEL: Record<Account, string> = {
  operator: "operator",
  desktop: "the desktop's account",
  root: "root",
};

/** terminalTitle is a terminal's name: the account and the program. */
export function terminalTitle(terminal: Terminal): string {
  const session = terminal.session;
  if (session === undefined) return "Terminal";
  const command = session.command === "" ? "shell" : (session.command.split("/").pop() ?? session.command);
  return `${session.user || "shell"} · ${command}`;
}

/** HostTerminalPage is a host's terminal place. */
export function HostTerminalPage(): JSX.Element {
  const shell = useShell();
  const domain = useHost();
  const tab = usePageTab();
  const [search, setSearch] = useSearchParams<{ attach?: string }>();
  const attached = () => search.attach ?? "";
  const listing = createLive(() => shell.rookery.terminals.list({ host: domain.name() }), { key: domain.name });
  const refresh = () => listing.refresh().catch(() => undefined);
  const terminals = () => listing()?.terminals ?? [];
  const running = () => terminals().filter((one) => one.session?.running);
  const shown = () => terminals().find((one) => one.session?.id === attached());

  const defaultAccount = (): Account => (domain.workstation() ? "desktop" : "operator");
  const accounts = (): Account[] => (domain.workstation() ? ["desktop", "operator", "root"] : ["operator", "root"]);

  // A new terminal shows here, or in a new tab of the workbench.
  const open = createAction(
    async (account: Account, place?: "here" | TabPlace) => {
      const opened = await shell.rookery.terminals.open({
        host: domain.name(),
        session: { user: { name: account }, cols: 120, rows: 32 },
      });
      const id = opened.terminal?.session?.id ?? "";
      if (id === "") throw new Error("The host opened no terminal.");
      await refresh();
      if (place === "here") setSearch({ attach: id });
      else shell.openTab(terminalPath(domain.name(), id), place);
    },
    { failed: "The terminal did not open" },
  );

  const end = createAction(
    async (id: string) => {
      await shell.rookery.terminals.close({ id });
      await refresh();
      if (id === untrack(attached)) setSearch({ attach: undefined }, { replace: true });
    },
    { failed: "The terminal did not end" },
  );

  const newIn = (place: "here" | TabPlace | undefined): ViewAction[] => [
    {
      id: "open",
      label: open.busy() ? "Opening…" : "New terminal",
      icon: "plus",
      disabled: open.busy(),
      onClick: () => void open.run(defaultAccount(), place),
    },
    ...accounts()
      .filter((account) => account !== defaultAccount())
      .map(
        (account): ViewAction => ({
          id: `open-${account}`,
          label: `New terminal as ${ACCOUNT_LABEL[account]}`,
          overflow: true,
          disabled: open.busy(),
          onClick: () => void open.run(account, place),
        }),
      ),
  ];
  const actions = (): ViewAction[] =>
    attached() === ""
      ? [
          ...newIn("here"),
          { id: "refresh", label: "Read the terminals again", icon: "refresh-cw", overflow: true, onClick: () => void refresh() },
        ]
      : [
          ...newIn(undefined),
          {
            id: "open-right",
            label: "New terminal to the right",
            overflow: true,
            disabled: open.busy(),
            onClick: () => void open.run(defaultAccount(), { edge: "right" }),
          },
          {
            id: "open-below",
            label: "New terminal below",
            overflow: true,
            disabled: open.busy(),
            onClick: () => void open.run(defaultAccount(), { edge: "bottom" }),
          },
          {
            id: "end",
            label: "End this terminal",
            overflow: true,
            danger: true,
            disabled: end.busy() || !shown()?.session?.running,
            onClick: () => void end.run(attached()),
          },
        ];

  const list = { label: "Terminal", href: hostPath(domain.name(), "terminal") };
  const crumbs = () =>
    attached() === ""
      ? domain.crumbs("Terminal")
      : domain.crumbs(list, shown() === undefined ? "Terminal" : terminalTitle(shown()!));

  return (
    <View
      id="host-terminal"
      crumbs={crumbs()}
      actions={actions()}
      scroll={attached() === ""}
      error={listing.error() === undefined ? undefined : `The terminals could not be read. ${errorMessage(listing.error())}`}
    >
      <Show
        when={attached() === ""}
        fallback={
          <For each={[attached()]}>
            {(id) => (
              <div class="shell-terminal">
                <TerminalPane id={id} active={tab.focused()} />
              </div>
            )}
          </For>
        }
      >
        <Show when={listing() !== undefined} fallback={<Skeleton width="30%" height="14px" />}>
          <Show
            when={running().length > 0}
            fallback={
              <Card>
                <EmptyState icon="terminal" title="No terminals">
                  <Button icon="plus" loading={open.busy()} onClick={() => void open.run(defaultAccount(), "here")}>
                    New terminal
                  </Button>
                </EmptyState>
              </Card>
            }
          >
            <Card padding="none">
              <RowList label={`Terminals on ${domain.name()}`}>
                <For each={running()} keyed={(terminal) => terminal.session?.id ?? ""}>
                  {(terminal) => (
                    <ListRow
                      data-terminal={terminal().session?.id}
                      icon="terminal"
                      title={terminalTitle(terminal())}
                      status={<State status="live">Running</State>}
                      href={terminalPath(domain.name(), terminal().session?.id ?? "")}
                    />
                  )}
                </For>
              </RowList>
            </Card>
          </Show>
        </Show>
      </Show>
    </View>
  );
}

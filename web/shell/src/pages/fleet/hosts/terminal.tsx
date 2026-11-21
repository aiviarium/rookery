// A host's terminals: every shell running on it, one tab each, tiled in
// Nocturne's Dock so several can be side by side or stacked.
//
// A terminal lives on its host and outlives the page, so the tabs are the
// host's running terminals, wherever they were opened. Where they sit is this
// device's: the layout is kept per host in the browser. A pane attaches when
// its tab is first shown; closing a tab ends that shell on the host.
import { Show, createEffect, createMemo, createSignal, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useSearchParams } from "@solidjs/router";
import type { Terminal } from "@rookery/proto";
import {
  Button,
  Callout,
  Dock,
  type DockLayout,
  EmptyState,
  Skeleton,
  type ViewAction,
  dockAdd,
  dockFrameOf,
  dockLayout,
  dockRemove,
  dockShow,
  dockSplit,
  dockSync,
} from "@aiviarium/nocturne";

import { createAction } from "../../../parts/action.ts";
import { createLive } from "../../../parts/data.ts";
import { View } from "../../../parts/view.tsx";
import { useShell } from "../../../shell.tsx";
import { TerminalPane } from "../terminals/pane.tsx";
import { useHost } from "./domain.tsx";

/** Account is who a new shell runs as on the host. */
export type Account = "operator" | "desktop" | "root";

const ACCOUNT_LABEL: Record<Account, string> = {
  operator: "operator",
  desktop: "the desktop's account",
  root: "root",
};

/** terminalTitle is a tab's label: the account and the program. */
export function terminalTitle(terminal: Terminal): string {
  const session = terminal.session;
  if (session === undefined) return "Terminal";
  const command = session.command === "" ? "shell" : (session.command.split("/").pop() ?? session.command);
  return `${session.user || "shell"} · ${command}`;
}

/** layoutKey is where this device keeps a host's terminal layout. */
const layoutKey = (host: string) => `rookery.terminals.${host}`;

/** savedLayout is the layout this device kept for a host, if it kept one that still reads. */
function savedLayout(host: string): DockLayout | undefined {
  try {
    const raw = localStorage.getItem(layoutKey(host));
    if (raw === null) return undefined;
    const parsed = JSON.parse(raw) as DockLayout;
    return Array.isArray(parsed.columns) && parsed.columns.length > 0 ? parsed : undefined;
  } catch {
    return undefined;
  }
}

function saveLayout(host: string, layout: DockLayout): void {
  try {
    localStorage.setItem(layoutKey(host), JSON.stringify(layout));
  } catch {
    // Without storage the layout lasts as long as the page.
  }
}

/** HostTerminalPage is a host's terminal place. */
export function HostTerminalPage(): JSX.Element {
  const shell = useShell();
  const domain = useHost();
  const host = untrack(domain.name);
  const [search, setSearch] = useSearchParams<{ attach?: string }>();
  const listing = createLive(() => shell.rookery.terminals.list({ host: domain.name() }), { key: domain.name });
  const refresh = () => listing.refresh().catch(() => undefined);

  // Exited shells stay as tabs only while this page has them open.
  const [kept, setKept] = createSignal<Terminal[]>([]);
  const terminals = createMemo(() => {
    const running = (listing()?.terminals ?? []).filter((one) => one.session?.running);
    const ids = new Set(running.map((one) => one.session?.id));
    return [...running, ...kept().filter((one) => !ids.has(one.session?.id))];
  });
  const byId = createMemo(() => new Map(terminals().map((one) => [one.session?.id ?? "", one])));
  const ids = createMemo(() => terminals().map((one) => one.session?.id ?? ""), {
    equals: (a, b) => a.length === b.length && a.every((id, index) => id === b[index]),
  });

  const [layout, setLayoutSignal] = createSignal<DockLayout>(savedLayout(host) ?? dockLayout());
  const setLayout = (next: DockLayout) => {
    setLayoutSignal(next);
    saveLayout(host, next);
  };

  // The dock holds the host's terminals: new ones open in the focused frame, ended ones go.
  createEffect(
    () => (listing() === undefined ? undefined : ids()),
    (current) => {
      if (current !== undefined) setLayout(dockSync(untrack(layout), current));
    },
  );
  // The terminal the address names is shown, once it is one of the tabs.
  createEffect(
    () => [ids(), search.attach ?? ""] as const,
    ([current, wanted]) => {
      if (wanted === "" || !current.includes(wanted)) return;
      const frame = dockFrameOf(untrack(layout), wanted);
      if (frame !== undefined && frame.active !== wanted) setLayout(dockShow(untrack(layout), wanted));
    },
  );
  const changeLayout = (next: DockLayout) => {
    setLayout(next);
    if (search.attach !== undefined) setSearch({ attach: undefined });
  };

  const defaultAccount = (): Account => (domain.workstation() ? "desktop" : "operator");
  const open = createAction(
    async (account: Account, place?: { frame: string; edge?: "right" | "bottom" }) => {
      const opened = await shell.rookery.terminals.open({
        host: domain.name(),
        session: { user: { name: account }, cols: 120, rows: 32 },
      });
      const terminal = opened.terminal;
      const id = terminal?.session?.id ?? "";
      if (terminal === undefined || id === "") throw new Error("The host opened no terminal.");
      setKept([...kept(), terminal]);
      let next = dockAdd(layout(), id, place?.frame);
      if (place?.edge !== undefined) next = dockSplit(next, id, place.frame, place.edge);
      setLayout(next);
      await refresh();
    },
    { failed: "The terminal did not open" },
  );

  const close = createAction(
    async (id: string) => {
      await shell.rookery.terminals.close({ id });
      setKept(kept().filter((one) => one.session?.id !== id));
      setLayout(dockRemove(layout(), id));
      await refresh();
    },
    { failed: "The terminal did not close" },
  );

  const accounts = (): Account[] => (domain.workstation() ? ["desktop", "operator", "root"] : ["operator", "root"]);
  const actions = (): ViewAction[] => [
    {
      id: "open",
      label: open.busy() ? "Opening…" : "New terminal",
      icon: "plus",
      disabled: open.busy(),
      onClick: () => void open.run(defaultAccount()),
    },
    ...accounts()
      .filter((account) => account !== defaultAccount())
      .map(
        (account): ViewAction => ({
          id: `open-${account}`,
          label: `New terminal as ${ACCOUNT_LABEL[account]}`,
          overflow: true,
          disabled: open.busy(),
          onClick: () => void open.run(account),
        }),
      ),
    { id: "refresh", label: "Read the terminals again", icon: "refresh-cw", overflow: true, onClick: () => void refresh() },
  ];

  return (
    <View id="host-terminal" crumbs={domain.crumbs("Terminal")} actions={actions()} scroll={false}>
      <Show when={listing.error()}>
        {(error) => <Callout tone="danger">{error().message}</Callout>}
      </Show>
      <Show when={listing() !== undefined} fallback={<Skeleton width="30%" height="14px" />}>
        <Show
          when={terminals().length > 0}
          fallback={
            <EmptyState icon="terminal" title="No terminals">
              <Button icon="plus" loading={open.busy()} onClick={() => void open.run(defaultAccount())}>
                New terminal
              </Button>
            </EmptyState>
          }
        >
          <Dock
            class="shell-terminals"
            label={`Terminals on ${domain.name()}`}
            layout={layout()}
            onLayoutChange={changeLayout}
            tab={(id) => {
              const one = byId().get(id);
              return {
                label: one === undefined ? "Terminal" : terminalTitle(one),
                icon: "terminal",
                detail: one === undefined || one.session?.running ? undefined : `exited ${one.session?.exitCode ?? 0}`,
              };
            }}
            onClose={(id) => void close.run(id)}
            closeLabel={(item) => `End ${item.label}`}
            onNewTab={(frame) => void open.run(defaultAccount(), { frame })}
            onSplit={(frame, edge) => void open.run(defaultAccount(), { frame, edge })}
          >
            {(id, shown) => <LazyPane id={id} shown={shown} />}
          </Dock>
        </Show>
      </Show>
    </View>
  );
}

/** LazyPane attaches a terminal the first time its tab is shown, and keeps it attached while it is a tab. */
function LazyPane(props: { id: string; shown: () => boolean }): JSX.Element {
  const [seen, setSeen] = createSignal(untrack(props.shown));
  createEffect(props.shown, (shown) => {
    if (shown) setSeen(true);
  });
  return (
    <Show when={seen()}>
      <TerminalPane id={props.id} active={props.shown()} />
    </Show>
  );
}

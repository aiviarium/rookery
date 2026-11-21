// A host's terminals: every shell running on it, one tab each, drawn by
// xterm.js over the whole frame.
//
// A terminal lives on its host and outlives the page, so the tabs are the
// host's running terminals, wherever they were opened. A pane attaches when its
// tab is first shown; closing a tab ends that shell on the host.
import { For, Show, createEffect, createMemo, createSignal, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useSearchParams } from "@solidjs/router";
import type { Terminal } from "@rookery/proto";
import { Button, Callout, EmptyState, Skeleton, TabStrip, type ViewAction } from "@aiviarium/nocturne";

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

/** HostTerminalPage is a host's terminal place. */
export function HostTerminalPage(): JSX.Element {
  const shell = useShell();
  const domain = useHost();
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
  const ids = createMemo(() => terminals().map((one) => one.session?.id ?? ""), {
    equals: (a, b) => a.length === b.length && a.every((id, index) => id === b[index]),
  });
  const [active, setActive] = createSignal("");
  // A pane attaches when its tab is first shown, and stays attached while it is a tab.
  const [shown, setShown] = createSignal<ReadonlySet<string>>(new Set());
  const show = (id: string) => {
    setActive(id);
    setShown((previous) => (previous.has(id) ? previous : new Set([...previous, id])));
    if (search.attach !== undefined && search.attach !== id) setSearch({ attach: undefined });
  };

  // The tab the address names, or the first one, is the one shown.
  createEffect(
    () => [ids(), search.attach ?? ""] as const,
    ([ids, wanted]) => {
      if (wanted !== "" && ids.includes(wanted)) {
        if (untrack(active) !== wanted) show(wanted);
        return;
      }
      if (!ids.includes(untrack(active)) && ids.length > 0) show(ids[0]!);
    },
  );

  const defaultAccount = (): Account => (domain.workstation() ? "desktop" : "operator");
  const open = createAction(
    async (account: Account) => {
      const opened = await shell.rookery.terminals.open({
        host: domain.name(),
        session: { user: { name: account }, cols: 120, rows: 32 },
      });
      const terminal = opened.terminal;
      const id = terminal?.session?.id ?? "";
      if (terminal === undefined || id === "") throw new Error("The host opened no terminal.");
      setKept([...kept(), terminal]);
      await refresh();
      show(id);
    },
    { failed: "The terminal did not open" },
  );

  const close = createAction(
    async (id: string) => {
      await shell.rookery.terminals.close({ id });
      setKept(kept().filter((one) => one.session?.id !== id));
      const next = new Set(shown());
      next.delete(id);
      setShown(next);
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
          <div class="shell-terminals" data-part="host-terminals">
            <TabStrip
              label={`Terminals on ${domain.name()}`}
              items={terminals().map((one) => ({
                id: one.session?.id ?? "",
                label: terminalTitle(one),
                detail: one.session?.running ? undefined : `exited ${one.session?.exitCode ?? 0}`,
              }))}
              value={active()}
              onChange={show}
              onClose={(id) => void close.run(id)}
              closeLabel={(item) => `End ${item.label}`}
              onNewTab={() => void open.run(defaultAccount())}
            />
            <div class="shell-terminals__panes">
              {/* Keyed by id: a refresh brings new records, and must not attach a pane again. */}
              <For each={ids().filter((id) => shown().has(id))}>
                {(id) => (
                  <div class="shell-terminals__pane" hidden={id !== active()}>
                    <TerminalPane id={id} active={id === active()} />
                  </div>
                )}
              </For>
            </div>
          </div>
        </Show>
      </Show>
    </View>
  );
}

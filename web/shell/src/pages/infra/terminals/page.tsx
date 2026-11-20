// Terminals: several hosts' PTYs at once, each in a tab, each drawn by xterm.js.
//
// A terminal lives on its host and outlives the page, so the tabs are only what
// this browser has open: one closed here can be attached again from the host's
// list, and one opened elsewhere appears there too.
import { For, Show, createEffect, createSignal, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useParams } from "@solidjs/router";
import { createQuery } from "@rookery/client/solid";
import type { Terminal } from "@rookery/proto";
import {
  Button,
  Callout,
  Card,
  EmptyState,
  ListRow,
  RowList,
  Select,
  TabStrip,
} from "@aiviarium/nocturne";

import { useShell } from "../../../shell.tsx";
import { Page } from "../../../parts/page.tsx";
import { errorMessage } from "../format.ts";
import { TerminalPane } from "./pane.tsx";

/** OpenTab is one terminal this page has open. */
interface OpenTab {
  readonly key: string;
  readonly host: string;
  readonly title: string;
}

type Account = "desktop" | "operator" | "root";

export function TerminalsPage(): JSX.Element {
  const shell = useShell();
  const params = useParams<{ id?: string }>();
  const hosts = createQuery(() => shell.rookery.hosts.list({ limit: 200 }));

  const [host, setHost] = createSignal(new URLSearchParams(location.search).get("host") ?? "");
  const [account, setAccount] = createSignal<Account>("desktop");
  const [tabs, setTabs] = createSignal<OpenTab[]>([]);
  const [active, setActive] = createSignal("");
  const [existing, setExisting] = createSignal<Terminal[]>([]);
  const [opening, setOpening] = createSignal(false);
  const [failure, setFailure] = createSignal<string>();

  type Session = NonNullable<Terminal["session"]>;

  const titleOf = (session: Session, name: string): string => {
    const command = session.command === "" ? "shell" : (session.command.split("/").pop() ?? session.command);
    return `${name} · ${command}`;
  };

  const open = (id: string, name: string, title: string): void => {
    setTabs(previous => (previous.some(tab => tab.key === id) ? previous : [...previous, { key: id, host: name, title }]));
    setActive(id);
  };

  const loadExisting = async (name: string): Promise<void> => {
    if (name === "") {
      setExisting([]);
      return;
    }
    try {
      const listed = await shell.rookery.terminals.list({ host: name });
      setExisting(listed.terminals);
    } catch {
      setExisting([]);
    }
  };

  // The first host is the one the page opens on, once the list has arrived: a
  // Select needs a value that is in its options.
  createEffect(
    () => hosts()?.hosts[0]?.name,
    (first) => {
      if (first === undefined) return;
      if (host() === "") setHost(first);
      void loadExisting(host());
    },
  );

  onSettled(() => {
    const direct = params.id ?? "";
    if (direct !== "") open(direct, host(), "Terminal");
  });

  const create = async (): Promise<void> => {
    const name = host();
    if (name === "" || opening()) return;
    setOpening(true);
    setFailure(undefined);
    try {
      const opened = await shell.rookery.terminals.open({
        host: name,
        session: { user: { name: account() }, cols: 100, rows: 30 },
      });
      const session = opened.terminal?.session;
      if (opened.terminal === undefined || session === undefined) throw new Error("The host opened no terminal.");
      open(session.id, name, titleOf(session, name));
      await loadExisting(name);
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setOpening(false);
    }
  };

  const close = async (id: string): Promise<void> => {
    setTabs(previous => previous.filter(tab => tab.key !== id));
    if (active() === id) setActive(tabs()[0]?.key ?? "");
    try {
      await shell.rookery.terminals.close({ id });
    } catch (error) {
      setFailure(errorMessage(error));
    }
    await loadExisting(host());
  };

  return (
    <Page
      title="Terminals"
      description="A shell on a host, kept there while this page is closed. Several can be open at once."
      actions={() => [
        { id: "refresh", label: "Read the terminals again", icon: "refresh-cw", onClick: () => void loadExisting(host()) },
        {
          id: "open",
          label: opening() ? "Opening…" : "Open a terminal",
          primary: true,
          disabled: host() === "" || opening(),
          onClick: () => void create(),
        },
      ]}
    >
      <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
      <div class="shell-terminals-layout">
        <div class="nc-stack">
          <Card title="A new terminal">
            <div class="nc-stack">
              <Select
                label="Host"
                value={host()}
                options={(hosts()?.hosts ?? []).map(one => ({ value: one.name, label: one.name }))}
                onChange={chosen => {
                  setHost(chosen);
                  void loadExisting(chosen);
                }}
              />
              <Select
                label="As"
                value={account()}
                options={[
                  { value: "desktop", label: "desktop — the desktop's account" },
                  { value: "operator", label: "operator — needs manage on the host" },
                  { value: "root", label: "root — needs manage on the host" },
                ]}
                onChange={chosen => setAccount(chosen as Account)}
              />
              <Button variant="secondary" onClick={() => void create()} disabled={host() === "" || opening()}>
                Open
              </Button>
            </div>
          </Card>
          <Card title="On this host" count={existing().length}>
            <Show
              when={existing().length > 0}
              fallback={<EmptyState size="sm" title="No terminals" description="None is running on this host." />}
            >
              <RowList label="Terminals on this host" variant="inline">
                <For each={existing()}>
                  {terminal => (
                    <Show when={terminal.session}>
                      {session => (
                        <ListRow
                          data-terminal={session().id}
                          title={<span class="nc-code">{session().user}</span>}
                          secondary={session().running ? "running" : `exited ${session().exitCode}`}
                          onClick={() => open(session().id, terminal.host, titleOf(session(), terminal.host))}
                        />
                      )}
                    </Show>
                  )}
                </For>
              </RowList>
            </Show>
          </Card>
        </div>
        <div class="shell-terminals">
          <Show
            when={tabs().length > 0}
            fallback={
              <Card fill padding="none">
                <EmptyState
                  title="No terminal is open"
                  description="Choose a host, and open a shell on it. It keeps running when this page closes."
                />
              </Card>
            }
          >
            <TabStrip
              label="Terminals"
              items={tabs().map(tab => ({ id: tab.key, label: tab.title }))}
              value={active()}
              onChange={setActive}
              onClose={id => void close(id)}
            />
            <div class="shell-terminals__panes">
              <For each={tabs()}>
                {tab => (
                  <div class="shell-terminals__pane" style={{ display: tab.key === active() ? "block" : "none" }}>
                    <TerminalPane id={tab.key} active={tab.key === active()} />
                  </div>
                )}
              </For>
            </div>
          </Show>
        </div>
      </div>
    </Page>
  );
}

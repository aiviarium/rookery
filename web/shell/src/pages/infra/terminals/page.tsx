// Terminals: several hosts' PTYs at once, each in a tab, each drawn by xterm.js.
//
// A terminal lives on its host and outlives the page, so the tabs are only what
// this browser has open: one closed here can be attached again from the host's
// list, and one opened elsewhere appears there too.
import { For, Show, createSignal, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useParams } from "@solidjs/router";
import { createQuery } from "@rookery/client/solid";
import type { Terminal } from "@rookery/proto";

import { useShell } from "../../../shell.tsx";
import { Button, Callout, Card, Empty, Field, IconButton, Page, Select, Spinner } from "../../../ui.tsx";
import { errorMessage } from "../format.ts";
import { TabStrip } from "../parts.tsx";
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

  onSettled(() => {
    const chosen = host() === "" ? (hosts()?.hosts[0]?.name ?? "") : host();
    setHost(chosen);
    void loadExisting(chosen);
    const direct = params.id ?? "";
    if (direct !== "") open(direct, chosen, "Terminal");
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
      actions={
        <>
          <IconButton label="Read the terminals again" glyph="jobs" onClick={() => void loadExisting(host())} />
          <Button variant="primary" onClick={() => void create()} disabled={host() === "" || opening()}>
            {opening() ? "Opening…" : "Open a terminal"}
          </Button>
        </>
      }
    >
      <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
      <div class="infra-terminals-layout">
        <div class="nc-stack">
          <Card title="A new terminal">
            <div class="nc-stack">
              <Field label="Host">
                <Select
                  value={host()}
                  options={(hosts()?.hosts ?? []).map(one => ({ value: one.name, label: one.name }))}
                  onChange={chosen => {
                    setHost(chosen);
                    void loadExisting(chosen);
                  }}
                />
              </Field>
              <Field label="As">
                <Select
                  value={account()}
                  options={[
                    { value: "desktop", label: "desktop — the desktop's account" },
                    { value: "operator", label: "operator — needs manage on the host" },
                    { value: "root", label: "root — needs manage on the host" },
                  ]}
                  onChange={chosen => setAccount(chosen as Account)}
                />
              </Field>
              <Button variant="secondary" onClick={() => void create()} disabled={host() === "" || opening()}>
                Open
              </Button>
            </div>
          </Card>
          <Card title="On this host">
            <p class="nc-meta">{existing().length} {existing().length === 1 ? "terminal" : "terminals"}</p>
            <Show
              when={existing().length > 0}
              fallback={<Empty title="No terminals" description="None is running on this host." />}
            >
              <ul class="nc-list">
                <For each={existing()}>
                  {terminal => (
                    <Show when={terminal.session}>
                      {session => (
                        <li class="nc-list__row" data-terminal={session().id}>
                          <button
                            type="button"
                            class="infra-link"
                            onClick={() => open(session().id, terminal.host, titleOf(session(), terminal.host))}
                          >
                            <span class="nc-code">{session().user}</span>
                            <span class="nc-meta">{session().running ? "running" : `exited ${session().exitCode}`}</span>
                          </button>
                        </li>
                      )}
                    </Show>
                  )}
                </For>
              </ul>
            </Show>
          </Card>
        </div>
        <div class="infra-terminals">
          <Show
            when={tabs().length > 0}
            fallback={
              <Empty
                title="No terminal is open"
                description="Choose a host, and open a shell on it. It keeps running when this page closes."
              />
            }
          >
            <TabStrip
              label="Terminals"
              items={tabs().map(tab => ({ id: tab.key, label: tab.title }))}
              value={active()}
              onChange={setActive}
              onClose={id => void close(id)}
            />
            <div class="infra-terminals__panes">
              <For each={tabs()}>
                {tab => (
                  <div class="infra-terminals__pane" style={{ display: tab.key === active() ? "block" : "none" }}>
                    <TerminalPane id={tab.key} active={tab.key === active()} />
                  </div>
                )}
              </For>
            </div>
          </Show>
        </div>
      </div>
      <Show when={hosts.pending() && tabs().length === 0}>
        <span class="nc-row">
          <Spinner /> Reading the hosts…
        </span>
      </Show>
    </Page>
  );
}

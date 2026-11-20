// A host's port forwards: a port on the host's loopback, reached through the
// server at `/hosts/{name}/ports/{port}/`. The list is the person's own, kept in
// this browser, because a forward is a shortcut and not something the server
// needs to know.
//
// A port's page opens in a tab of its own: the proxy answers on the server's
// origin, so it sits beside the shell rather than inside it.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";

import { Button, Callout, Card, Empty, Field, IconButton, Input } from "../../../ui.tsx";

/** PortForward is one saved port on a host. */
interface PortForward {
  readonly port: number;
  readonly label?: string;
}

const KEY = "rookery.shell.forwards.v1";

/** loadForwards reads every host's forwards from this browser. */
function loadForwards(): Record<string, PortForward[]> {
  try {
    const stored: unknown = JSON.parse(localStorage.getItem(KEY) ?? "null");
    if (typeof stored === "object" && stored !== null) return stored as Record<string, PortForward[]>;
  } catch {
    // A browser that refuses storage keeps the forwards for this visit only.
  }
  return {};
}

/** portPath is where the proxy serves a port on a host. */
export function portPath(host: string, port: number): string {
  return `/hosts/${encodeURIComponent(host)}/ports/${port}/`;
}

export function HostPorts(props: { host: string }): JSX.Element {
  const [forwards, setForwards] = createSignal<Record<string, PortForward[]>>(loadForwards());
  const [port, setPort] = createSignal("");
  const [label, setLabel] = createSignal("");
  const [failure, setFailure] = createSignal<string>();

  const saved = () => forwards()[props.host] ?? [];

  const save = (next: Record<string, PortForward[]>): void => {
    setForwards(next);
    try {
      localStorage.setItem(KEY, JSON.stringify(next));
    } catch {
      // The list still works for this visit.
    }
  };

  const add = (): void => {
    const value = Number(port());
    if (!Number.isInteger(value) || value <= 0 || value > 65535) {
      setFailure("A port is a number from 1 to 65535.");
      return;
    }
    const lists = { ...forwards() };
    const others = (lists[props.host] ?? []).filter(forward => forward.port !== value);
    const forward: PortForward = label().trim() === "" ? { port: value } : { port: value, label: label().trim() };
    lists[props.host] = [...others, forward].sort((a, b) => a.port - b.port);
    save(lists);
    setPort("");
    setLabel("");
    setFailure(undefined);
  };

  const forget = (value: number): void => {
    const lists = { ...forwards() };
    const left = (lists[props.host] ?? []).filter(forward => forward.port !== value);
    if (left.length === 0) delete lists[props.host];
    else lists[props.host] = left;
    save(lists);
  };

  return (
    <Card title="Ports">
      <form
        class="nc-row"
        onSubmit={event => {
          event.preventDefault();
          add();
        }}
      >
        <Field label="Port">
          <Input value={port()} placeholder="5173" onInput={setPort} />
        </Field>
        <Field label="What runs there">
          <Input value={label()} placeholder="dev server" onInput={setLabel} />
        </Field>
        <Button type="submit" disabled={port() === ""}>
          Add forward
        </Button>
      </form>
      <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
      <Show
        when={saved().length > 0}
        fallback={
          <Empty
            title="No forwards yet"
            description={`A port a program on ${props.host} listens on, such as a dev server, opened through the server.`}
          />
        }
      >
        <ul class="nc-list">
          <For each={saved()}>
            {forward => (
              <li class="nc-list__row" data-port={forward.port}>
                <span class="nc-code">{forward.port}</span>
                <span class="infra-grow">{forward.label ?? ""}</span>
                <Button
                  size="sm"
                  onClick={() => window.open(portPath(props.host, forward.port), "_blank", "noopener,noreferrer")}
                >
                  Open
                </Button>
                <IconButton label={`Forget the port ${forward.port}`} glyph="close" onClick={() => forget(forward.port)} />
              </li>
            )}
          </For>
        </ul>
      </Show>
      <p class="nc-muted">
        A forward opens in a tab of its own. The server reaches only ports on {props.host}'s loopback.
      </p>
    </Card>
  );
}

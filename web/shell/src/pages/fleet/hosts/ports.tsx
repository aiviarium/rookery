// A host's port forwards: a port on the host's loopback, reached through the
// server at `/hosts/{name}/ports/{port}/`. The list is the person's own, kept in
// this browser, because a forward is a shortcut and not something the server
// needs to know.
//
// A port's page opens in a tab of its own: the proxy answers on the server's
// origin, so it sits beside the shell rather than inside it.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Button, Card, EmptyState, ListRow, RowList, TextField } from "@aiviarium/nocturne";

import { createAction } from "../../../parts/action.ts";
import { FormDialog } from "../../../parts/dialogs.tsx";
import { RowActions } from "../../../parts/row-actions.tsx";
import { View } from "../../../parts/view.tsx";
import { hostCrumbs, useHost } from "./domain.tsx";

/** PortForward is one saved port on a host. */
export interface PortForward {
  readonly port: number;
  readonly label?: string;
}

const KEY = "rookery.shell.forwards.v1";

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

/** parsePort reads a port number, or says why it is not one. */
export function parsePort(text: string): number | string {
  const value = Number(text.trim());
  if (text.trim() === "" || !Number.isInteger(value) || value <= 0 || value > 65535) return "A port is a number from 1 to 65535.";
  return value;
}

/** HostPortsPage is a host's ports place. */
export function HostPortsPage(): JSX.Element {
  const domain = useHost();
  const [forwards, setForwards] = createSignal<Record<string, PortForward[]>>(loadForwards());
  const [adding, setAdding] = createSignal(false);
  const [port, setPort] = createSignal("");
  const [label, setLabel] = createSignal("");

  const saved = () => forwards()[domain.name()] ?? [];
  const save = (next: Record<string, PortForward[]>): void => {
    setForwards(next);
    try {
      localStorage.setItem(KEY, JSON.stringify(next));
    } catch {
      // The list still works for this visit.
    }
  };
  const open = (value: number) => window.open(portPath(domain.name(), value), "_blank", "noopener,noreferrer");

  const add = createAction(async () => {
    const value = parsePort(port());
    if (typeof value === "string") throw new Error(value);
    const lists = { ...forwards() };
    const others = (lists[domain.name()] ?? []).filter((forward) => forward.port !== value);
    const forward: PortForward = label().trim() === "" ? { port: value } : { port: value, label: label().trim() };
    lists[domain.name()] = [...others, forward].sort((a, b) => a.port - b.port);
    save(lists);
  });

  const forget = (value: number): void => {
    const lists = { ...forwards() };
    const left = (lists[domain.name()] ?? []).filter((forward) => forward.port !== value);
    if (left.length === 0) delete lists[domain.name()];
    else lists[domain.name()] = left;
    save(lists);
  };

  return (
    <View
      id="host-ports"
      crumbs={hostCrumbs(domain.name(), "Ports")}
      actions={[{ id: "add", label: "Add port", icon: "plus", onClick: () => setAdding(true) }]}
    >
      <Card padding="none">
        <Show
          when={saved().length > 0}
          fallback={
            <EmptyState icon="plug" title="No ports">
              <Button icon="plus" onClick={() => setAdding(true)}>
                Add port
              </Button>
            </EmptyState>
          }
        >
          <RowList label="Ports">
            <For each={saved()}>
              {(forward) => (
                <ListRow
                  data-port={forward.port}
                  icon="plug"
                  title={<span class="nc-mono">{forward.port}</span>}
                  secondary={forward.label}
                  detail={<span class="nc-mono">{portPath(domain.name(), forward.port)}</span>}
                  onClick={() => open(forward.port)}
                  menu={[
                    { id: "open", label: "Open", icon: "arrow-up-right" },
                    { id: "forget", label: "Forget", icon: "trash", danger: true },
                  ]}
                  menuLabel={`Actions for port ${forward.port}`}
                  onMenuSelect={(id) => (id === "open" ? open(forward.port) : forget(forward.port))}
                  persistentActions={
                    <RowActions
                      label={`Actions for port ${forward.port}`}
                      items={[
                        { id: "open", label: "Open", icon: "arrow-up-right" },
                        { type: "separator" },
                        { id: "forget", label: "Forget", icon: "trash", danger: true },
                      ]}
                      onSelect={(id) => (id === "open" ? open(forward.port) : forget(forward.port))}
                    />
                  }
                />
              )}
            </For>
          </RowList>
        </Show>
      </Card>
      <FormDialog
        open={adding()}
        title="Add port"
        submit="Add"
        action={add}
        disabled={port().trim() === ""}
        size="sm"
        onClose={() => {
          setAdding(false);
          setPort("");
          setLabel("");
        }}
      >
        <TextField label="Port" info="A port on the host's loopback, opened through the server in a tab of its own. The list is kept in this browser." inputmode="numeric" value={port()} placeholder="5173" onInput={(event) => setPort(event.currentTarget.value)} />
        <TextField label="What runs there" value={label()} placeholder="dev server" onInput={(event) => setLabel(event.currentTarget.value)} />
      </FormDialog>
    </View>
  );
}

// What these pages draw that the shell's own `ui.tsx` does not have yet: a
// table, tabs, a dialog, a property list, a log and the pieces of state around
// them.
//
// TODO(infra): each part here is Nocturne's markup and classes, so a page reads
// the same now and after the port: swap it for Nocturne's `DataTable`, `Tabs`,
// `Dialog`, `PropertyList`, `LogViewer`, `State` and `TabStrip` once
// `@aiviarium/nocturne`'s `solid-2` branch is the pinned dependency. None of
// them is a copy of a Nocturne component: they are plain elements with Nocturne's
// classes until those components can be imported.
import { For, Show, createEffect, createSignal, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";

import { cx } from "../../ui.tsx";

/** Tone is how a state reads: the dot's kind and, for warn and error, the word's. */
export type Tone = "live" | "ok" | "warn" | "error" | "idle" | "off" | "neutral";

/** StateText is a dot and a word, the one way a page shows a state. */
export function StateText(props: { tone: Tone; children?: JSX.Element }): JSX.Element {
  return (
    <span class="nc-state" data-tone={props.tone}>
      <span class={cx("nc-dot", props.tone !== "neutral" && `nc-dot--${props.tone}`)} aria-hidden="true" />
      {props.children}
    </span>
  );
}

/** Column is one column of a Table. */
export interface Column<T> {
  /** id is stable, and names the column for a test. */
  id: string;
  /** header is the column's heading. */
  header: string;
  /** value is the cell as text, and what a test reads. */
  value: (row: T) => string;
  /** cell draws the cell; `value` is used when it is absent. */
  cell?: (row: T) => JSX.Element;
  /** mono sets the ID and path treatment. */
  mono?: boolean;
  /** num right-aligns a number. */
  num?: boolean;
}

/**
 * Table is a table of rows in columns. A row that opens something is clickable.
 */
export function Table<T>(props: {
  label: string;
  rows: readonly T[];
  columns: readonly Column<T>[];
  rowKey: (row: T) => string;
  onRowClick?: (row: T) => void;
  rowActions?: (row: T) => JSX.Element;
  empty?: string;
}): JSX.Element {
  return (
    <Show
      when={props.rows.length > 0}
      fallback={<p class="nc-empty__desc">{props.empty ?? "Nothing here."}</p>}
    >
      <div class="infra-table-wrap">
        <table class="nc-table infra-table" aria-label={props.label}>
          <thead>
            <tr>
              <For each={props.columns}>
                {column => (
                  <th scope="col" class={cx(column.num && "is-num")}>
                    {column.header}
                  </th>
                )}
              </For>
              <Show when={props.rowActions !== undefined}>
                <th scope="col" />
              </Show>
            </tr>
          </thead>
          <tbody>
            <For each={props.rows}>
              {row => (
                <tr
                  data-row={props.rowKey(row)}
                  class={cx(props.onRowClick !== undefined && "nc-table__row--clickable")}
                  onClick={() => props.onRowClick?.(row)}
                >
                  <For each={props.columns}>
                    {column => (
                      <td class={cx(column.mono && "nc-code", column.num && "is-num")}>
                        {column.cell?.(row) ?? column.value(row)}
                      </td>
                    )}
                  </For>
                  <Show when={props.rowActions !== undefined}>
                    <td class="infra-table__actions">{props.rowActions?.(row)}</td>
                  </Show>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </div>
    </Show>
  );
}

/** TabItem is one tab: its id and what it says. */
export interface TabItem {
  readonly id: string;
  readonly label: string;
  readonly count?: number;
}

/** Tabs is a row of tabs over one section of a page. */
export function Tabs(props: {
  label: string;
  items: readonly TabItem[];
  value: string;
  onChange: (id: string) => void;
}): JSX.Element {
  return (
    <div class="nc-tabs" role="tablist" aria-label={props.label}>
      <For each={props.items}>
        {item => (
          <button
            type="button"
            class="nc-tab"
            role="tab"
            aria-selected={props.value === item.id ? "true" : "false"}
            data-tab={item.id}
            onClick={() => props.onChange(item.id)}
          >
            {item.label}
            <Show when={(item.count ?? 0) > 0}>
              <span class="nc-tab__count">{item.count}</span>
            </Show>
          </button>
        )}
      </For>
    </div>
  );
}

/** Dialog is a modal form or message, closed by Escape or its buttons. */
export function Dialog(props: {
  open: boolean;
  onClose: () => void;
  title: string;
  description?: string;
  footer?: JSX.Element;
  size?: "sm" | "md" | "lg";
  children?: JSX.Element;
}): JSX.Element {
  onSettled(() => {
    if (!props.open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") props.onClose();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  });

  return (
    <Show when={props.open}>
      <div
        class="infra-scrim"
        style={{ position: "fixed", inset: "0", "z-index": "80", background: "var(--bg-0)", opacity: "0.82" }}
        onClick={() => props.onClose()}
      />
      <div
        class={cx("nc-dialog", `nc-dialog--${props.size ?? "md"}`)}
        role="dialog"
        aria-modal="true"
        aria-label={props.title}
        style={{ position: "fixed", inset: "0", margin: "auto", "z-index": "81", height: "fit-content", "max-height": "90vh", overflow: "auto" }}
      >
        <header class="nc-dialog__header">
          <h2 class="nc-dialog__title">{props.title}</h2>
        </header>
        <div class="nc-dialog__body">
          <Show when={props.description !== undefined}>
            <p class="nc-dialog__desc">{props.description}</p>
          </Show>
          <div class="nc-stack">{props.children}</div>
        </div>
        <Show when={props.footer !== undefined}>
          <footer class="nc-dialog__footer">{props.footer}</footer>
        </Show>
      </div>
    </Show>
  );
}

/** PropertyItem is one label-and-value pair. */
export interface PropertyItem {
  readonly label: string;
  readonly value?: JSX.Element | string;
}

/** PropertyList is label-and-value pairs, a row each. */
export function PropertyList(props: { items: readonly PropertyItem[] }): JSX.Element {
  return (
    <dl class="infra-properties">
      <For each={props.items}>
        {item => (
          <>
            <dt class="nc-muted">{item.label}</dt>
            <dd>{item.value === undefined || item.value === "" ? "—" : item.value}</dd>
          </>
        )}
      </For>
    </dl>
  );
}

/** LogEntry is one line of a job's or an action's output. */
export interface LogEntry {
  readonly id: string;
  readonly text: string;
  readonly at?: string;
}

/** Log is an output that follows its end, in a monospaced block. */
export function Log(props: {
  entries: readonly LogEntry[];
  title?: string;
  state?: string;
  empty?: string;
  actions?: JSX.Element;
}): JSX.Element {
  let body: HTMLPreElement | undefined;
  const follow = () => {
    if (body === undefined) return;
    body.scrollTop = body.scrollHeight;
  };
  createEffect(
    () => props.entries.length,
    () => queueMicrotask(follow),
  );

  return (
    <section class="nc-card infra-log" aria-label={props.title ?? "Output"}>
      <header class="nc-panel-header">
        <h2 class="nc-panel-title">{props.title ?? "Output"}</h2>
        <div class="nc-panel-header__actions">
          <Show when={props.state !== undefined}>
            <span class="nc-meta">{props.state}</span>
          </Show>
          {props.actions}
        </div>
      </header>
      <pre
        class="nc-code infra-log__body"
        ref={element => {
          body = element;
        }}
      >
        <Show
          when={props.entries.length > 0}
          fallback={<span class="nc-muted">{props.empty ?? "No output yet."}</span>}
        >
          <For each={props.entries}>
            {entry => (
              <div class="nc-code__line" data-line={entry.id}>
                <Show when={entry.at !== undefined}>
                  <span class="nc-meta infra-log__time">{new Date(entry.at!).toLocaleTimeString()}</span>
                </Show>
                <span class="nc-code__text">{entry.text}</span>
              </div>
            )}
          </For>
        </Show>
      </pre>
    </section>
  );
}

/** TabStrip is a row of closable tabs, for the terminals. */
export function TabStrip(props: {
  label: string;
  items: readonly { id: string; label: string }[];
  value: string;
  onChange: (id: string) => void;
  onClose: (id: string) => void;
}): JSX.Element {
  return (
    <div class="nc-tabstrip" role="tablist" aria-label={props.label}>
      <div class="nc-tabstrip__tabs">
        <For each={props.items}>
          {item => (
            <span class="nc-tabstrip__tab" aria-selected={props.value === item.id ? "true" : "false"}>
              <button type="button" class="nc-tabstrip__title" role="tab" onClick={() => props.onChange(item.id)}>
                {item.label}
              </button>
              <button
                type="button"
                class="nc-tabstrip__close"
                aria-label={`Close ${item.label}`}
                onClick={() => props.onClose(item.id)}
              >
                ×
              </button>
            </span>
          )}
        </For>
      </div>
    </div>
  );
}

/** TextArea is a multi-line field, for a pasted key or a clipboard. */
export function TextArea(props: {
  label: string;
  value: string;
  rows?: number;
  onInput: (value: string) => void;
}): JSX.Element {
  return (
    <div class="nc-field">
      <label class="nc-fieldrow__label">{props.label}</label>
      <textarea
        class="nc-input"
        rows={props.rows ?? 3}
        value={props.value}
        onInput={event => props.onInput(event.currentTarget.value)}
      />
    </div>
  );
}

/** useCopy is a "copied" flag that turns itself off. */
export function useCopy(): [() => boolean, (text: string) => void] {
  const [copied, setCopied] = createSignal(false);
  return [
    copied,
    (text) => {
      void navigator.clipboard?.writeText(text).catch(() => undefined);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    },
  ];
}

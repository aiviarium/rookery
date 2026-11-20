// The elements the shell draws with.
//
// They are Nocturne's parts in all but the import: the classes are Nocturne's
// (`nc-*`), and every colour comes from its tokens. The components themselves
// are local only until Nocturne's Solid 2 branch is published; then this file
// re-exports `@aiviarium/nocturne`'s Button, TextField, Menu and the rest, and
// the pages that import here do not change.
//
// TODO(shell): replace each part below with the Nocturne component of the same
// name once `@aiviarium/nocturne`'s `solid-2` branch is the pinned dependency.
import { type Accessor, type Component, For, Show, createSignal, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";

/** cx joins class names and the objects and arrays Solid 2 allows in `class`. */
export function cx(...parts: unknown[]): string {
  const names: string[] = [];
  for (const part of parts) {
    if (part === undefined || part === null || part === false || part === "") continue;
    if (typeof part === "string") {
      names.push(part);
    } else if (Array.isArray(part)) {
      const nested = cx(...part);
      if (nested !== "") names.push(nested);
    } else if (typeof part === "object") {
      for (const [name, on] of Object.entries(part)) if (on) names.push(name);
    }
  }
  return names.join(" ");
}

/** GlyphName is one of the icons the shell draws. */
export type GlyphName =
  | "bell"
  | "check"
  | "chevron-down"
  | "close"
  | "database"
  | "document"
  | "gauge"
  | "groups"
  | "home"
  | "hosts"
  | "jobs"
  | "key"
  | "menu"
  | "plugin"
  | "plus"
  | "search"
  | "server"
  | "settings"
  | "sign-out"
  | "space"
  | "tailnet";

// The paths are drawn on a 24-unit grid, stroked with the current colour, which
// is what Nocturne's own icons do.
const GLYPHS: Record<GlyphName, string> = {
  bell: "M6 8a6 6 0 1 1 12 0c0 5 2 6 2 6H4s2-1 2-6 M9 20a3 3 0 0 0 6 0",
  check: "M4 12.5 9 18 20 6",
  "chevron-down": "M6 9l6 6 6-6",
  close: "M6 6l12 12M18 6 6 18",
  database: "M4 6c0-2 16-2 16 0v12c0 2-16 2-16 0z M4 6c0 2 16 2 16 0 M4 12c0 2 16 2 16 0",
  document: "M7 3h7l5 5v13H7z M14 3v5h5",
  gauge: "M4 18a8 8 0 1 1 16 0 M12 18l4-5",
  groups: "M8 11a3 3 0 1 0 0-6 3 3 0 0 0 0 6 M2 20c0-3 3-5 6-5s6 2 6 5 M16 11a3 3 0 1 0 0-6 M15 15c3 0 7 1 7 5",
  home: "M4 11 12 4l8 7v9h-6v-6h-4v6H4z",
  hosts: "M4 4h16v6H4z M4 14h16v6H4z M8 7h.01 M8 17h.01",
  jobs: "M12 3v3 M12 18v3 M4.2 7.5l2.6 1.5 M17.2 15l2.6 1.5 M4.2 16.5l2.6-1.5 M17.2 9l2.6-1.5 M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8",
  key: "M14 10a4 4 0 1 0-3.5 4L9 15.5V18H6v3H3v-3l7.5-7.5",
  menu: "M4 7h16 M4 12h16 M4 17h16",
  plugin: "M9 4v3 M15 4v3 M5 7h14v6a7 7 0 0 1-14 0z M12 20v-2",
  plus: "M12 5v14 M5 12h14",
  search: "M10.5 17a6.5 6.5 0 1 0 0-13 6.5 6.5 0 0 0 0 13z M15.5 15.5 20 20",
  server: "M4 4h16v6H4z M4 14h16v6H4z M8 7h.01 M8 17h.01",
  settings: "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6 M19 12l2-1-2-4-2 1a7 7 0 0 0-2-1l-.5-2h-5L9 7a7 7 0 0 0-2 1L5 7l-2 4 2 1a7 7 0 0 0 0 2l-2 1 2 4 2-1a7 7 0 0 0 2 1l.5 2h5l.5-2a7 7 0 0 0 2-1l2 1 2-4-2-1z",
  "sign-out": "M15 8V5H5v14h10v-3 M10 12h11 M18 9l3 3-3 3",
  space: "M4 8a8 8 0 1 1 16 0c0 6-8 12-8 12S4 14 4 8z M12 10a2 2 0 1 0 0-4 2 2 0 0 0 0 4",
  tailnet: "M5 12h4 M15 12h4 M9 12a3 3 0 1 0 6 0 3 3 0 0 0-6 0 M2 9v6 M22 9v6",
};

/** Glyph draws one icon at the size its container sets. */
export function Glyph(props: { name: GlyphName; size?: number; class?: string }): JSX.Element {
  return (
    <svg
      class={cx("nc-icon", props.class)}
      width={props.size ?? 16}
      height={props.size ?? 16}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="1.6"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d={GLYPHS[props.name]} />
    </svg>
  );
}

/** ButtonVariant is what a button does: the flow's action, an ordinary one, a
 * quiet one, or one that destroys. */
export type ButtonVariant = "primary" | "secondary" | "ghost" | "danger";

/** ButtonProps is what a button takes. */
export interface ButtonProps {
  variant?: ButtonVariant;
  size?: "sm" | "md";
  type?: "button" | "submit";
  disabled?: boolean;
  loading?: boolean;
  title?: string;
  class?: string;
  onClick?: (event: MouseEvent) => void;
  children?: JSX.Element;
}

/** Button is an action with a label that says what happens. */
export function Button(props: ButtonProps): JSX.Element {
  return (
    <button
      type={props.type ?? "button"}
      class={cx("nc-btn", `nc-btn--${props.variant ?? "secondary"}`, props.size === "sm" && "nc-btn--sm", props.class)}
      disabled={props.disabled === true || props.loading === true}
      title={props.title}
      onClick={(event) => props.onClick?.(event)}
    >
      <Show when={props.loading === true}>
        <Spinner />
      </Show>
      {props.children}
    </button>
  );
}

/** IconButton is a quiet action drawn as its icon, with a label a reader hears. */
export function IconButton(props: {
  label: string;
  glyph: GlyphName;
  onClick?: (event: MouseEvent) => void;
  expanded?: boolean;
  class?: string;
}): JSX.Element {
  return (
    <button
      type="button"
      class={cx("nc-icon-btn", props.class)}
      aria-label={props.label}
      title={props.label}
      aria-expanded={props.expanded === undefined ? undefined : props.expanded ? "true" : "false"}
      aria-haspopup={props.expanded === undefined ? undefined : "menu"}
      onClick={(event) => props.onClick?.(event)}
    >
      <Glyph name={props.glyph} />
    </button>
  );
}

/** Field wraps a labelled control. */
export function Field(props: {
  label: string;
  hint?: string;
  error?: string;
  children?: JSX.Element;
  for?: string;
}): JSX.Element {
  // The control is inside the label, so a reader hears the label whatever a
  // page gives its fields.
  return (
    <label class="nc-field" for={props.for}>
      <span class="nc-fieldrow__label">{props.label}</span>
      {props.children}
      <Show when={props.error !== undefined}>
        <p class="nc-fieldrow__message">{props.error}</p>
      </Show>
      <Show when={props.error === undefined && props.hint !== undefined}>
        <p class="nc-fieldrow__message">{props.hint}</p>
      </Show>
    </label>
  );
}

/** Input is a single-line text field. */
export function Input(props: {
  id?: string;
  type?: "text" | "email" | "password" | "search" | "url" | "number";
  value?: string;
  placeholder?: string;
  disabled?: boolean;
  autocomplete?: string;
  name?: string;
  class?: string;
  onInput?: (value: string) => void;
  onKeyDown?: (event: KeyboardEvent) => void;
}): JSX.Element {
  return (
    <input
      id={props.id}
      name={props.name}
      type={props.type ?? "text"}
      class={cx("nc-input", props.class)}
      value={props.value ?? ""}
      placeholder={props.placeholder}
      disabled={props.disabled === true}
      autocomplete={props.autocomplete}
      onInput={(event) => props.onInput?.(event.currentTarget.value)}
      onKeyDown={(event) => props.onKeyDown?.(event)}
    />
  );
}

/** Select is a choice out of a list. */
export function Select(props: {
  id?: string;
  value?: string;
  options: readonly { value: string; label: string }[];
  disabled?: boolean;
  class?: string;
  onChange?: (value: string) => void;
}): JSX.Element {
  return (
    <select
      id={props.id}
      class={cx("nc-select", "nc-input", props.class)}
      value={props.value ?? ""}
      disabled={props.disabled === true}
      onChange={(event) => props.onChange?.(event.currentTarget.value)}
    >
      <For each={props.options}>{option => <option value={option.value}>{option.label}</option>}</For>
    </select>
  );
}

/** Switch is a boolean that applies at once. */
export function Switch(props: {
  label: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (checked: boolean) => void;
}): JSX.Element {
  return (
    <label class="nc-row nc-row--control">
      <input
        type="checkbox"
        class="nc-switch"
        checked={props.checked}
        disabled={props.disabled === true}
        onChange={(event) => props.onChange(event.currentTarget.checked)}
      />
      <span>{props.label}</span>
    </label>
  );
}

/** Badge is a short state, coloured by what it says. */
export function Badge(props: {
  tone?: "neutral" | "success" | "warning" | "danger" | "info" | "accent" | "outline";
  children?: JSX.Element;
}): JSX.Element {
  return (
    <span class={cx("nc-badge", props.tone !== undefined && props.tone !== "neutral" && `nc-badge--${props.tone}`)}>
      {props.children}
    </span>
  );
}

/** Avatar is a person's initials. */
export function Avatar(props: { name: string; size?: "sm" | "lg" }): JSX.Element {
  return (
    <span class={cx("nc-avatar", props.size !== undefined && `nc-avatar--${props.size}`)} title={props.name}>
      {initials(props.name)}
    </span>
  );
}

/** initials is the one or two letters an avatar shows. */
export function initials(name: string): string {
  const words = name.trim().split(/\s+/).filter((word) => word !== "");
  const first = words[0]?.[0] ?? "?";
  const last = words.length > 1 ? (words[words.length - 1]?.[0] ?? "") : "";
  return (first + last).toUpperCase();
}

/** Card is a bordered surface. */
export function Card(props: { title?: string; actions?: JSX.Element; class?: string; children?: JSX.Element }): JSX.Element {
  return (
    <section class={cx("nc-card", props.class)}>
      <Show when={props.title !== undefined || props.actions !== undefined}>
        <header class="nc-panel-header">
          <h2 class="nc-panel-title">{props.title}</h2>
          <Show when={props.actions !== undefined}>
            <div class="nc-panel-header__actions">{props.actions}</div>
          </Show>
        </header>
      </Show>
      <div class="nc-pad">{props.children}</div>
    </section>
  );
}

/** Callout is a note about what happened, in the tone of what happened. */
export function Callout(props: { tone?: "info" | "warning" | "danger"; children?: JSX.Element }): JSX.Element {
  return <div class={cx("nc-callout", `nc-callout--${props.tone ?? "info"}`)}>{props.children}</div>;
}

/** Empty is a place with nothing in it yet, and what fills it. */
export function Empty(props: { title: string; description?: string; actions?: JSX.Element }): JSX.Element {
  return (
    <div class="nc-empty">
      <p class="nc-empty__title">{props.title}</p>
      <Show when={props.description !== undefined}>
        <p class="nc-empty__desc">{props.description}</p>
      </Show>
      <Show when={props.actions !== undefined}>
        <div class="nc-empty__actions">{props.actions}</div>
      </Show>
    </div>
  );
}

/** Spinner is work in progress. */
export function Spinner(): JSX.Element {
  return <span class="nc-spinner" role="status" aria-label="Loading" />;
}

/** SectionHeading is a heading over a group of rows. */
export function SectionHeading(props: { children?: JSX.Element }): JSX.Element {
  return <h2 class="nc-section-heading">{props.children}</h2>;
}

/** Page is the body of a view: a title, what it is, and what it holds. */
export function Page(props: {
  title: string;
  description?: string;
  actions?: JSX.Element;
  children?: JSX.Element;
}): JSX.Element {
  return (
    <div class="nc-stack" style={{ gap: "var(--space-4)", padding: "var(--space-2) var(--space-2) var(--space-4)" }}>
      <header class="nc-row" style={{ "justify-content": "space-between", "align-items": "flex-start" }}>
        <div class="nc-stack">
          <h1 class="nc-type-title">{props.title}</h1>
          <Show when={props.description !== undefined}>
            <p class="nc-muted">{props.description}</p>
          </Show>
        </div>
        <Show when={props.actions !== undefined}>
          <div class="nc-row">{props.actions}</div>
        </Show>
      </header>
      {props.children}
    </div>
  );
}

/** Popover is a surface under its trigger that a click outside closes. */
export function Popover(props: {
  open: boolean;
  onClose: () => void;
  align?: "start" | "end";
  label: string;
  children?: JSX.Element;
}): JSX.Element {
  let surface: HTMLDivElement | undefined;
  onSettled(() => {
    const onPointerDown = (event: PointerEvent) => {
      if (!props.open) return;
      const target = event.target;
      if (!(target instanceof Node)) return;
      // The trigger is outside the surface, and it closes the menu itself; a
      // click anywhere else closes it here.
      if (surface !== undefined && !surface.contains(target) && !isTrigger(target)) props.onClose();
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (props.open && event.key === "Escape") props.onClose();
    };
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  });

  return (
    <Show when={props.open}>
      <div
        ref={(element) => {
          surface = element;
        }}
        class="nc-popover"
        role="dialog"
        aria-label={props.label}
        style={{
          position: "absolute",
          top: "calc(100% + var(--space-1))",
          [props.align === "start" ? "left" : "right"]: "0",
          "z-index": "60",
          "min-width": "220px",
        }}
      >
        {props.children}
      </div>
    </Show>
  );
}

/** isTrigger reports whether an element opens a popover. */
function isTrigger(element: Node): boolean {
  const closest = element instanceof Element ? element.closest("[aria-haspopup]") : null;
  return closest !== null;
}

/** relative makes a popover's anchor. */
export const relative: JSX.CSSProperties = { position: "relative" };

/**
 * useToggle is a boolean a button flips, for a popover's `open`.
 */
export function useToggle(initial = false): [Accessor<boolean>, (next?: boolean) => void] {
  const [open, setOpen] = createSignal(initial);
  return [open, (next?: boolean) => setOpen((current) => next ?? !current)];
}

/** Tabs is a row of sections, one of them open. */
export function Tabs(props: {
  active: string;
  tabs: readonly { id: string; label: string }[];
  onSelect: (id: string) => void;
}): JSX.Element {
  return (
    <div class="nc-tabs" role="tablist">
      <For each={props.tabs}>
        {tab => (
          <button
            type="button"
            class="nc-tab"
            role="tab"
            aria-selected={props.active === tab.id ? "true" : "false"}
            onClick={() => props.onSelect(tab.id)}
          >
            {tab.label}
          </button>
        )}
      </For>
    </div>
  );
}

/** Table is a list of rows with named columns. */
export function Table(props: { columns: readonly string[]; children?: JSX.Element }): JSX.Element {
  return (
    <div style={{ overflow: "auto" }}>
      <table class="nc-table">
        <thead>
          <tr>
            <For each={props.columns}>{column => <th scope="col">{column}</th>}</For>
          </tr>
        </thead>
        <tbody>{props.children}</tbody>
      </table>
    </div>
  );
}

/** Textarea is a field for text longer than a line. */
export function Textarea(props: {
  id?: string;
  value?: string;
  rows?: number;
  placeholder?: string;
  class?: string;
  onInput?: (value: string) => void;
}): JSX.Element {
  return (
    <textarea
      id={props.id}
      class={cx("nc-textarea", props.class)}
      rows={props.rows ?? 6}
      placeholder={props.placeholder}
      onInput={(event) => props.onInput?.(event.currentTarget.value)}
    >
      {props.value ?? ""}
    </textarea>
  );
}

/** Code is a value a reader copies, in a monospace block. */
export function Code(props: { children?: JSX.Element }): JSX.Element {
  return <pre class="nc-code">{props.children}</pre>;
}

/** ComponentOf is a component with no props, as a page module exports one. */
export type ComponentOf = Component<Record<string, never>>;

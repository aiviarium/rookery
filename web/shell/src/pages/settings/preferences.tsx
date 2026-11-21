// Preferences: every setting the core and the plugins declare, one section per
// plugin, as rows with their control. The scope (you, this space, the server)
// is chosen above the rows and kept in the address; a row shows the value that
// applies at that scope, says where it comes from when the scope does not set
// it, and resets with its "Modified" chip. A change applies at once.
import { For, Show, createEffect, createMemo, createSignal, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useSearchParams } from "@solidjs/router";
import { cbor } from "@rookery/client";
import type { Setting } from "@rookery/proto";
import {
  Callout,
  Card,
  CodeBlock,
  Dialog,
  EmptyState,
  FilterBar,
  NumberField,
  Select,
  SettingRow,
  Skeleton,
  Switch,
  Tabs,
  TextArea,
  TextField,
} from "@aiviarium/nocturne";

import { createAction } from "../../parts/action.ts";
import { createLive } from "../../parts/data.ts";
import { errorMessage } from "../../parts/errors.ts";
import { View } from "../../parts/view.tsx";
import { useShell } from "../../shell.tsx";
import { CORE, SCOPES, type Scope, changed, modifiedAt, sectionPath, settingsVersion, useScope } from "./scope.ts";

/** FROM names a scope a value is inherited from. */
const FROM: Record<string, string> = { user: "From you", space: "From this space", global: "From Server", default: "Default" };

/** INHERITS are the scopes a scope falls back to, nearest first. */
const INHERITS: Record<Scope, Scope[]> = { user: ["user", "space", "global"], space: ["space", "global"], global: ["global"] };

/** atScope is the value that applies at a scope and where it comes from. */
export function atScope(setting: Setting, scope: Scope): { value: Uint8Array; from: string } {
  for (const candidate of INHERITS[scope]) {
    const value = setting.valuesCbor[candidate];
    if (value !== undefined && setting.scopes.includes(candidate)) return { value, from: candidate };
  }
  return { value: setting.defaultCbor, from: "default" };
}

/** useSettings is every setting, read again when one changes here or the space does. */
export function useSettings() {
  const shell = useShell();
  const settings = createLive(() => shell.rookery.settings.list({}), { key: settingsVersion, space: true });
  const plugins = createLive(() => shell.rookery.plugins.list({}).catch(() => undefined));
  const titleOf = (plugin: string) =>
    plugin === CORE ? "General" : plugins()?.plugins.find((one) => one.id === plugin)?.name || plugin;
  /** sections are the plugins that declare settings: the core first, then by name. */
  const sections = createMemo(() => {
    const seen = new Set((settings()?.settings ?? []).map((setting) => setting.plugin));
    return [...seen].sort((a, b) => (a === CORE ? -1 : b === CORE ? 1 : titleOf(a).localeCompare(titleOf(b))));
  });
  return { settings, sections, titleOf };
}

/** SCOPE_NOTE says what a value at a scope does. */
function scopeNote(scope: Scope, space: string, admin: boolean): string {
  if (scope === "user") return "Your own values. They win over the space's and the server's.";
  if (scope === "space") return `Values for everyone in ${space}. A person's own values still win.`;
  return admin ? "Values for everyone on this server, unless a space or a person sets their own." : "Values for everyone on this server. Only admins change them.";
}

/** PreferencesPage is the settings of one section, or of all of them. */
export function PreferencesPage(props: { plugin?: string }): JSX.Element {
  const shell = useShell();
  const { scope, setScope } = useScope();
  const [search, setSearch] = useSearchParams<{ q?: string; modified?: string }>();
  const { settings, sections, titleOf } = useSettings();
  const space = () => shell.spaces.current();
  const [query, setQuery] = createSignal(untrack(() => search.q ?? ""));
  const [json, setJson] = createSignal(false);
  const onlyModified = () => search.modified === "1";

  const inView = (setting: Setting) => props.plugin === undefined || setting.plugin === props.plugin;
  const matches = (setting: Setting): boolean => {
    if (!inView(setting)) return false;
    if (onlyModified() && !modifiedAt(setting, scope())) return false;
    const words = query().trim().toLowerCase();
    if (words === "") return true;
    return [setting.key, setting.title, setting.description, setting.category].some((text) => text.toLowerCase().includes(words));
  };
  const shown = createMemo(() =>
    sections()
      .map((plugin) => ({
        plugin,
        rows: (settings()?.settings ?? [])
          .filter((setting) => setting.plugin === plugin && matches(setting))
          .sort((a, b) => a.category.localeCompare(b.category) || (a.title || a.key).localeCompare(b.title || b.key)),
      }))
      .filter((section) => section.rows.length > 0),
  );
  const modifiedHere = () => (settings()?.settings ?? []).filter((setting) => inView(setting) && modifiedAt(setting, scope())).length;
  const scopeLabel = () => SCOPES.find((one) => one.id === scope())!.label;
  const asJson = () => {
    const out: Record<string, unknown> = {};
    for (const setting of settings()?.settings ?? []) {
      const value = setting.valuesCbor[scope()];
      if (inView(setting) && value !== undefined) out[setting.key] = cbor.decode(value);
    }
    return JSON.stringify(out, null, 2);
  };
  const title = () => (props.plugin === undefined ? "All settings" : titleOf(props.plugin));

  return (
    <View
      id="settings"
      crumbs={[{ label: "Settings", href: props.plugin === undefined ? undefined : sectionPath() }, ...(props.plugin === undefined ? [] : [{ label: title() }])]}
      reading
      actions={[
        { id: "refresh", label: "Refresh", icon: "refresh-cw", overflow: true, onClick: changed },
        { id: "json", label: "Open as JSON", icon: "file", overflow: true, onClick: () => setJson(true) },
      ]}
    >
      <div class="nc-stack" data-part="settings">
        <div class="shell-scope" data-part="settings-scope">
          <Tabs
            label="Scope"
            value={scope()}
            onChange={(next) => setScope(next as Scope)}
            items={SCOPES.map((one) => ({ id: one.id, label: one.label, icon: one.icon, disabled: one.id === "space" && space() === "" }))}
          />
          <p class="nc-meta">
            {scopeNote(scope(), shell.spaces.currentSpace()?.name ?? "this space", shell.signIn.user()?.admin === true)}
            <Show when={space() === "" && scope() === "user"}> Choose a space in the sidebar's header to set values for it.</Show>
          </p>
        </div>
        <FilterBar
          label="Settings filters"
          search={
            <TextField
              variant="search"
              aria-label="Search settings"
              placeholder={props.plugin === undefined ? "Search settings" : `Search ${title()}`}
              value={query()}
              onInput={(event) => {
                const next = event.currentTarget.value;
                setQuery(next);
                setSearch({ q: next === "" ? undefined : next }, { replace: true });
              }}
            />
          }
          end={<Switch label={`Modified only (${modifiedHere()})`} checked={onlyModified()} onChange={(on) => setSearch({ modified: on ? "1" : undefined })} />}
        />
        <Show when={settings.error()}>
          {(failure) => <Callout tone="danger">The settings could not be read. {errorMessage(failure())}</Callout>}
        </Show>
        <Show when={settings()} fallback={<Skeleton width="50%" />}>
          <Show
            when={shown().length > 0}
            fallback={
              <Card>
                <EmptyState
                  icon={query() === "" && !onlyModified() ? "settings" : "search"}
                  title={onlyModified() ? `Nothing is modified at ${scopeLabel()}` : query() === "" ? "No settings" : "No setting matches"}
                  description={query() === "" && !onlyModified() ? "The core and each installed plugin declare the settings they take." : undefined}
                />
              </Card>
            }
          >
            <For each={shown()} keyed={(section) => section.plugin}>
              {(section) => (
                <Card
                  title={titleOf(section().plugin)}
                  count={section().rows.length}
                  padding="none"
                  data-plugin={section().plugin}
                >
                  <For each={section().rows} keyed={(setting) => setting.key}>
                    {(setting) => <Row setting={setting()} section={titleOf(section().plugin)} scope={scope()} space={space()} />}
                  </For>
                </Card>
              )}
            </For>
          </Show>
        </Show>
      </div>
      <Dialog open={json()} onClose={() => setJson(false)} size="lg" title={`${title()} set at ${scopeLabel()}`}>
        <CodeBlock language="json" lineNumbers={false} code={asJson()} />
      </Dialog>
    </View>
  );
}

/** Row is one setting at the scope shown. */
function Row(props: { setting: Setting; section: string; scope: Scope; space: string }): JSX.Element {
  const shell = useShell();
  const has = () => props.setting.scopes.includes(props.scope);
  const editable = () => has() && props.setting.editable.includes(props.scope);
  const modified = () => props.setting.valuesCbor[props.scope] !== undefined;
  const shown = () => atScope(props.setting, props.scope);
  const value = () => cbor.decode<unknown>(shown().value);
  const target = () => ({ key: props.setting.key, scope: props.scope, space: props.scope === "space" ? props.space : "" });

  const set = createAction(async (next: unknown) => {
    await shell.rookery.settings.set({ ...target(), valueCbor: cbor.encode(next) });
    changed();
  }, { failed: "The setting was not changed" });
  const reset = createAction(async () => {
    await shell.rookery.settings.reset(target());
    changed();
  }, { failed: "The setting was not reset" });

  const note = (): string | undefined => {
    if (!has()) return `Not set per ${props.scope === "user" ? "person" : props.scope === "space" ? "space" : "server"}`;
    if (modified()) return undefined;
    return FROM[shown().from];
  };

  return (
    <SettingRow
      data-key={props.setting.key}
      title={props.setting.title || props.setting.key}
      description={props.setting.description || undefined}
      name={props.setting.key}
      category={props.setting.category === "" || props.setting.category === props.section ? undefined : props.setting.category}
      modified={modified()}
      onReset={editable() ? () => void reset.run() : undefined}
      inherited={note()}
      muted={!has()}
      control={(ids) => <Control setting={props.setting} value={value()} disabled={!editable() || set.busy()} labelId={ids.labelId} onSet={(next) => void set.run(next)} />}
    />
  );
}

/** Control is the field a setting's type takes. A typed value is saved when the field is left. */
function Control(props: { setting: Setting; value: unknown; disabled: boolean; labelId: string; onSet: (value: unknown) => void }): JSX.Element {
  const type = () => props.setting.type;
  return (
    <Show
      when={type() !== "bool"}
      fallback={<Switch aria-labelledby={props.labelId} checked={props.value === true} disabled={props.disabled} onChange={(on) => props.onSet(on)} />}
    >
      <Show
        when={props.setting.choices.length === 0}
        fallback={
          <Select
            aria-labelledby={props.labelId}
            value={String(props.value ?? "")}
            disabled={props.disabled}
            options={props.setting.choices.map((choice) => ({ value: choice, label: choice }))}
            onChange={(next) => next !== props.value && props.onSet(next)}
          />
        }
      >
        <Show
          when={type() !== "int" && type() !== "float"}
          fallback={
            <NumberField
              aria-labelledby={props.labelId}
              value={typeof props.value === "number" ? props.value : null}
              integer={type() === "int"}
              min={props.setting.minimum}
              max={props.setting.maximum}
              unit={props.setting.unit || undefined}
              required
              disabled={props.disabled}
              onCommit={(next) => next !== null && next !== props.value && props.onSet(next)}
            />
          }
        >
          <TextControl {...props} />
        </Show>
      </Show>
    </Show>
  );
}

/** TextControl edits a string, a time or a CBOR value as JSON, keeping the draft while it is typed. */
function TextControl(props: { setting: Setting; value: unknown; disabled: boolean; labelId: string; onSet: (value: unknown) => void }): JSX.Element {
  const json = () => props.setting.type === "cbor";
  const text = (value: unknown) => (json() ? JSON.stringify(value ?? null, null, 2) : value === undefined || value === null ? "" : String(value));
  const [draft, setDraft] = createSignal(untrack(() => text(props.value)));
  const [problem, setProblem] = createSignal<string>();
  // A value saved elsewhere, or a scope chosen above, replaces the draft.
  createEffect(
    () => text(props.value),
    (next) => {
      setDraft(next);
      setProblem(undefined);
    },
  );
  // A commit reads the field itself: a write to the draft shows only after
  // Solid flushes, and a blur can follow the last keystroke at once.
  const commit = (typed: string) => {
    if (typed === text(props.value)) return;
    if (!json()) {
      props.onSet(typed);
      return;
    }
    try {
      props.onSet(JSON.parse(typed));
      setProblem(undefined);
    } catch {
      setProblem("Not valid JSON.");
    }
  };
  return (
    <Show
      when={json()}
      fallback={
        <TextField
          aria-labelledby={props.labelId}
          mono={props.setting.type === "time"}
          value={draft()}
          disabled={props.disabled}
          onInput={(event) => setDraft(event.currentTarget.value)}
          onBlur={(event) => commit(event.currentTarget.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") commit(event.currentTarget.value);
            if (event.key === "Escape") setDraft(text(props.value));
          }}
        />
      }
    >
      <TextArea
        aria-labelledby={props.labelId}
        rows={3}
        value={draft()}
        error={problem()}
        disabled={props.disabled}
        onInput={(event) => setDraft(event.currentTarget.value)}
        onBlur={(event) => commit(event.currentTarget.value)}
      />
    </Show>
  );
}

// Preferences: every setting the core and the plugins declare that the person
// may change at a scope, one section per plugin, as rows with their control.
// In Settings the scope is a choice in the header, kept in the address, shown
// only when there is more than one; in a space it is the space. A row shows the
// value that applies, where it comes from when the scope does not set it, and
// resets with its "Modified" chip. A change applies at once.
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
  TextArea,
  TextField,
} from "@aiviarium/nocturne";

import { createAction } from "../../parts/action.ts";
import { createLive } from "../../parts/data.ts";
import { errorMessage } from "../../parts/errors.ts";
import { View } from "../../parts/view.tsx";
import { useShell } from "../../shell.tsx";
import { CORE, SCOPES, type Scope, changed, editableAt, modifiedAt, sectionPath, settingsVersion, useScope } from "./scope.ts";

/** FROM names a scope a value is inherited from. */
const FROM: Record<string, string> = { user: "From you", space: "From the space", global: "Set for everyone" };

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
  const settings = createLive(() => shell.rookery.settings.list({}), { key: settingsVersion });
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

/** PreferencesPage is the settings of one section, or of all of them, at Settings' scope or a space's. */
export function PreferencesPage(props: { plugin?: string; space?: string; crumbs?: Array<{ label: string; href?: string }> }): JSX.Element {
  const address = useScope();
  const [search, setSearch] = useSearchParams<{ q?: string; modified?: string }>();
  const { settings, sections, titleOf } = useSettings();
  const inSpace = () => props.space !== undefined;
  // The scopes the person may change anything at; a space's page has its own.
  const scopes = createMemo(() =>
    SCOPES.filter((one) => one.id === "user" || (settings()?.settings ?? []).some((setting) => editableAt(setting, one.id))),
  );
  const scope = (): Scope => (inSpace() ? "space" : scopes().some((one) => one.id === address.scope()) ? address.scope() : "user");
  const [query, setQuery] = createSignal(untrack(() => search.q ?? ""));
  const [json, setJson] = createSignal(false);
  const onlyModified = () => search.modified === "1";

  const inView = (setting: Setting) => (props.plugin === undefined || setting.plugin === props.plugin) && editableAt(setting, scope());
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
  const any = () => (settings()?.settings ?? []).some(inView);
  const asJson = () => {
    const out: Record<string, unknown> = {};
    for (const setting of settings()?.settings ?? []) {
      const value = setting.valuesCbor[scope()];
      if (inView(setting) && value !== undefined) out[setting.key] = cbor.decode(value);
    }
    return JSON.stringify(out, null, 2);
  };
  const title = () => (props.plugin === undefined ? "All settings" : titleOf(props.plugin));
  const crumbs = () =>
    props.crumbs ?? [{ label: "Settings", href: props.plugin === undefined ? undefined : sectionPath() }, ...(props.plugin === undefined ? [] : [{ label: title() }])];

  return (
    <View
      id="settings"
      crumbs={crumbs()}
      actions={[{ id: "json", label: "Open as JSON", icon: "file", overflow: true, onClick: () => setJson(true) }]}
    >
      <div class="nc-stack" data-part="settings">
        <Show when={any()}>
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
            end={
              <div class="nc-row">
                <Show when={!inSpace() && scopes().length > 1}>
                  <Select
                    aria-label="Scope"
                    data-part="settings-scope"
                    value={scope()}
                    options={scopes().map((one) => ({ value: one.id, label: one.label }))}
                    onChange={(next) => address.setScope(next as Scope)}
                  />
                </Show>
                <Switch label="Modified" checked={onlyModified()} onChange={(on) => setSearch({ modified: on ? "1" : undefined })} />
              </div>
            }
          />
        </Show>
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
                  title={onlyModified() && query() === "" ? "Nothing modified" : query() === "" ? "No settings" : "No setting matches"}
                />
              </Card>
            }
          >
            <For each={shown()} keyed={(section) => section.plugin}>
              {(section) => (
                <Card title={titleOf(section().plugin)} padding="none" data-plugin={section().plugin}>
                  <For each={section().rows} keyed={(setting) => setting.key}>
                    {(setting) => <Row setting={setting()} section={titleOf(section().plugin)} scope={scope()} space={props.space ?? ""} />}
                  </For>
                </Card>
              )}
            </For>
          </Show>
        </Show>
      </div>
      <Dialog open={json()} onClose={() => setJson(false)} size="lg" title={title()}>
        <CodeBlock language="json" lineNumbers={false} code={asJson()} />
      </Dialog>
    </View>
  );
}

/** Row is one setting at the scope shown. */
function Row(props: { setting: Setting; section: string; scope: Scope; space: string }): JSX.Element {
  const shell = useShell();
  const editable = () => editableAt(props.setting, props.scope);
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

  // A default value is the normal state and says nothing.
  const note = (): string | undefined => (modified() || shown().from === "default" ? undefined : FROM[shown().from]);

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

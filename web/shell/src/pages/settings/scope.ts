// What the settings places share: the scope in the address, which section a
// setting is in, and a version the pages bump after a change so the sidebar
// follows. Settings changes publish no events, so the shell keeps step itself.
import { createSignal } from "solid-js";
import { useSearchParams } from "@solidjs/router";
import type { Setting } from "@rookery/proto";

/** Scope is where a value is set. */
export type Scope = "user" | "space" | "global";

/** SCOPES name the scopes a person picks between in Settings. A space's are in the space. */
export const SCOPES: ReadonlyArray<{ id: Scope; label: string }> = [
  { id: "user", label: "You" },
  { id: "global", label: "Everyone" },
];

/** CORE is the section of the settings Rookery itself declares. */
export const CORE = "rookery";

const [version, setVersion] = createSignal(0);

/** settingsVersion changes whenever a setting is set or reset here. */
export const settingsVersion = version;

/** changed tells every settings place to read again. */
export function changed(): void {
  setVersion((value) => value + 1);
}

/** useScope is the scope in the address, and a way to change it. */
export function useScope(): { scope: () => Scope; setScope: (scope: Scope) => void; query: () => string } {
  const [search, setSearch] = useSearchParams<{ scope?: string }>();
  const scope = (): Scope => (search.scope === "global" ? "global" : "user");
  return {
    scope,
    setScope: (next) => setSearch({ scope: next === "user" ? undefined : next }),
    query: () => (scope() === "user" ? "" : `?scope=${scope()}`),
  };
}

/** editableAt is true when the caller may set the setting at the scope. */
export function editableAt(setting: Setting, scope: Scope): boolean {
  return setting.scopes.includes(scope) && setting.editable.includes(scope);
}

/** modifiedAt is true when the scope sets its own value. */
export function modifiedAt(setting: Setting, scope: Scope): boolean {
  return setting.valuesCbor[scope] !== undefined;
}

/** sectionPath is the place of a section of settings. */
export function sectionPath(plugin?: string): string {
  return plugin === undefined ? "/settings" : `/settings/p/${encodeURIComponent(plugin)}`;
}

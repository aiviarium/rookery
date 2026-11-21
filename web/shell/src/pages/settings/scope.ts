// What the settings places share: the scope in the address, which section a
// setting is in, and a version the pages bump after a change so the sidebar's
// counts follow. Settings changes publish no events, so the shell keeps step
// itself.
import { createSignal } from "solid-js";
import { useSearchParams } from "@solidjs/router";
import type { Setting } from "@rookery/proto";
import type { IconName } from "@aiviarium/nocturne";

import { useShell } from "../../shell.tsx";

/** Scope is where a value is set. */
export type Scope = "user" | "space" | "global";

/** SCOPES are the scopes in the order the switcher shows them. */
export const SCOPES: ReadonlyArray<{ id: Scope; label: string; icon: IconName }> = [
  { id: "user", label: "You", icon: "user" },
  { id: "space", label: "This space", icon: "layers" },
  { id: "global", label: "Server", icon: "server" },
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
  const shell = useShell();
  const [search, setSearch] = useSearchParams<{ scope?: string }>();
  const scope = (): Scope => {
    const wanted = search.scope;
    if (wanted === "space" && shell.spaces.current() === "") return "user";
    return wanted === "space" || wanted === "global" ? wanted : "user";
  };
  return {
    scope,
    setScope: (next) => setSearch({ scope: next === "user" ? undefined : next }),
    query: () => (scope() === "user" ? "" : `?scope=${scope()}`),
  };
}

/** modifiedAt is true when the scope sets its own value. */
export function modifiedAt(setting: Setting, scope: Scope): boolean {
  return setting.valuesCbor[scope] !== undefined;
}

/** sectionPath is the place of a section of settings. */
export function sectionPath(plugin?: string): string {
  return plugin === undefined ? "/settings" : `/settings/p/${encodeURIComponent(plugin)}`;
}

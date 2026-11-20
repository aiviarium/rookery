// The sidebar: the server's name, the space, and every page there is.
import { For, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useLocation } from "@solidjs/router";

import { useShell } from "../shell.tsx";
import { Glyph } from "../ui.tsx";
import { navigation } from "./navigation.ts";
import { SpaceSwitcher } from "./space-switcher.tsx";

/** Sidebar is the shell's navigation. */
export function Sidebar(): JSX.Element {
  const shell = useShell();
  const location = useLocation();
  const groups = () => navigation();

  const current = (href: string): boolean =>
    location.pathname === href || (href !== "/" && location.pathname.startsWith(`${href}/`));

  return (
    <aside class="nc-shell__sidebar">
      <div class="nc-sidebar-header">
        <div class="nc-sidebar-header__row">
          <a class="nc-sidebar-header__place" href="/" aria-label="Rookery, home">
            <span class="nc-sidebar-header__mark">
              <Glyph name="server" />
            </span>
            <span class="nc-sidebar-header__name">Rookery</span>
          </a>
        </div>
        <SpaceSwitcher />
      </div>

      <Show when={shell.plugins.error()}>
        {error => <p class="nc-meta">A plugin could not be loaded: {error().message}</p>}
      </Show>

      <nav class="nc-nav" aria-label="Main">
        <For each={groups()}>
          {group => (
            <>
              <Show when={group.title !== undefined}>
                <div class="nc-section-heading">{group.title}</div>
              </Show>
              <For each={group.items}>
                {item => (
                  <a class="nc-nav-item" href={item.href} aria-current={current(item.href) ? "page" : undefined}>
                    <Show when={item.icon !== undefined} fallback={<Glyph name="document" />}>
                      <Glyph name={item.icon ?? "document"} />
                    </Show>
                    <span class="nc-nav-item__label">{item.label}</span>
                  </a>
                )}
              </For>
            </>
          )}
        </For>
      </nav>
    </aside>
  );
}

// The space the shell is in, and the switch to another one.
import { For, Show } from "solid-js";
import type { JSX } from "@solidjs/web";

import { useShell } from "../shell.tsx";
import { Badge, Glyph, Popover, relative, useToggle } from "../ui.tsx";

/** SpaceSwitcher is the sidebar's top control: the current space, and the list. */
export function SpaceSwitcher(): JSX.Element {
  const shell = useShell();
  const [open, toggle] = useToggle();

  const label = () => shell.spaces.currentSpace()?.name ?? "All spaces";

  return (
    <div style={relative}>
      <button
        type="button"
        class="nc-nav-item"
        style={{ width: "100%" }}
        aria-haspopup="menu"
        aria-expanded={open() ? "true" : "false"}
        onClick={() => toggle()}
      >
        <Glyph name="space" />
        <span class="nc-nav-item__label">{label()}</span>
        <Glyph name="chevron-down" size={14} />
      </button>
      <Popover open={open()} onClose={() => toggle(false)} align="start" label="Spaces">
        <div class="nc-menu" role="menu">
          <div class="nc-menu__label">Spaces</div>
          <button
            type="button"
            class="nc-menu__item"
            role="menuitemradio"
            aria-checked={shell.spaces.current() === "" ? "true" : "false"}
            onClick={() => {
              shell.spaces.select("");
              toggle(false);
            }}
          >
            <span class="nc-nav-item__label">All spaces</span>
            <Show when={shell.spaces.current() === ""}>
              <Glyph name="check" size={14} />
            </Show>
          </button>
          <Show
            when={shell.spaces.list().length > 0}
            fallback={<div class="nc-menu__item nc-muted">No spaces yet.</div>}
          >
            <For each={shell.spaces.list()}>
              {space => (
                <button
                  type="button"
                  class="nc-menu__item"
                  role="menuitemradio"
                  aria-checked={shell.spaces.current() === space.id ? "true" : "false"}
                  onClick={() => {
                    shell.spaces.select(space.id);
                    toggle(false);
                  }}
                >
                  <span class="nc-nav-item__label">{space.name}</span>
                  <Badge tone="outline">{space.kind}</Badge>
                  <Show when={shell.spaces.current() === space.id}>
                    <Glyph name="check" size={14} />
                  </Show>
                </button>
              )}
            </For>
          </Show>
        </div>
      </Popover>
    </div>
  );
}

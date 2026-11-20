// Who is signed in, and how to stop being signed in.
import { Show } from "solid-js";
import type { JSX } from "@solidjs/web";

import { useShell } from "../shell.tsx";
import { Avatar, Badge, Glyph, Popover, relative, useToggle } from "../ui.tsx";

/** AccountMenu is the person's own menu at the top right. */
export function AccountMenu(): JSX.Element {
  const shell = useShell();
  const [open, toggle] = useToggle();

  const user = () => shell.signIn.user();
  const name = () => user()?.name || user()?.email || "Account";

  return (
    <div style={relative}>
      <button
        type="button"
        class="nc-btn nc-btn--ghost"
        aria-haspopup="menu"
        aria-expanded={open() ? "true" : "false"}
        aria-label="Account"
        onClick={() => toggle()}
      >
        <Avatar name={name()} size="sm" />
        <span class="nc-nav-item__label">{name()}</span>
        <Glyph name="chevron-down" size={14} />
      </button>
      <Popover open={open()} onClose={() => toggle(false)} label="Account">
        <div class="nc-menu" role="menu">
          <div class="nc-menu__label">{name()}</div>
          <Show when={user()?.email !== undefined && user()?.email !== ""}>
            <div class="nc-menu__item nc-muted">{user()?.email}</div>
          </Show>
          <Show when={user()?.admin === true}>
            <div class="nc-menu__item">
              <Badge tone="accent">admin</Badge>
            </div>
          </Show>
          <div class="nc-menu__sep" />
          <button
            type="button"
            class="nc-menu__item"
            role="menuitem"
            onClick={() => {
              toggle(false);
              void shell.signIn.signOut();
            }}
          >
            <Glyph name="sign-out" size={14} />
            Sign out
          </button>
        </div>
      </Popover>
    </div>
  );
}

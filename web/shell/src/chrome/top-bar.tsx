// The top of the content area: what page this is, and the person's own things.
import { useLocation } from "@solidjs/router";
import type { JSX } from "@solidjs/web";

import { useShell } from "../shell.tsx";
import { AccountMenu } from "./account-menu.tsx";
import { NotificationsMenu } from "./notifications-menu.tsx";
import { titleOf } from "./navigation.ts";

/** TopBar is the page's name and the account's controls. */
export function TopBar(): JSX.Element {
  const shell = useShell();
  const location = useLocation();

  return (
    <div class="nc-row" style={{ "justify-content": "space-between", width: "100%" }}>
      <div class="nc-row" style={{ gap: "var(--space-2)", "min-width": "0" }}>
        <span class="nc-type-label nc-muted">{shell.spaces.currentSpace()?.name ?? "All spaces"}</span>
        <span class="nc-meta">/</span>
        <strong class="nc-truncate">{titleOf(location.pathname)}</strong>
      </div>
      <div class="nc-row" style={{ gap: "var(--space-1)" }}>
        <NotificationsMenu />
        <AccountMenu />
      </div>
    </div>
  );
}

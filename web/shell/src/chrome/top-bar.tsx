// The app's header: what this is, and the person's own things.
import type { JSX } from "@solidjs/web";
import { Icon } from "@aiviarium/nocturne";

import { AccountMenu } from "./account-menu.tsx";
import { NotificationsMenu } from "./notifications-menu.tsx";

/** TopBar is the shell's own row: the product at the start, and the account's controls at the end. */
export function TopBar(): JSX.Element {
  return (
    <div class="shell-topbar">
      <span class="nc-row nc-row--tight">
        <Icon name="server" size="sm" />
        <span class="nc-type-label">Rookery</span>
      </span>
      <span class="nc-row nc-row--tight">
        <NotificationsMenu />
        <AccountMenu />
      </span>
    </div>
  );
}

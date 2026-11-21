// The person at the foot of the sidebar: who is signed in, the theme, and
// signing out.
import type { JSX } from "@solidjs/web";
import { Avatar, Badge, DropdownMenu, Icon, type MenuEntry } from "@aiviarium/nocturne";

import { useShell } from "../shell.tsx";
import { useChrome } from "./context.ts";

/** AccountMenu is the person's card, which opens their menu upwards. */
export function AccountMenu(): JSX.Element {
  const shell = useShell();
  const chrome = useChrome();

  const user = () => shell.signIn.user();
  const name = () => user()?.name || user()?.email || "Signed in";

  const items = (): MenuEntry[] => {
    const theme = shell.theme.choice();
    return [
      {
        type: "custom",
        content: (
          <div class="shell-account__who" data-part="account-who">
            <Avatar name={name()} size="lg" />
            <span class="shell-account__text">
              <span class="label nc-truncate">{name()}</span>
              <span class="nc-meta nc-truncate">{user()?.email}</span>
            </span>
            <Badge tone={user()?.admin ? "accent" : "neutral"}>{user()?.admin ? "Admin" : "Member"}</Badge>
          </div>
        ),
      },
      { type: "separator" },
      { id: "inbox", label: "Inbox", icon: "inbox" },
      { id: "settings", label: "Settings", icon: "settings" },
      { type: "separator" },
      { type: "label", label: "Theme" },
      { id: "theme.system", label: "System", checked: theme === "system" },
      { id: "theme.dark", label: "Dark", checked: theme === "dark" },
      { id: "theme.light", label: "Light", checked: theme === "light" },
      { type: "separator" },
      { id: "sign-out", label: "Sign out", danger: true },
    ];
  };

  const choose = (id: string) => {
    if (id === "inbox" || id === "settings") {
      shell.navigate(`/${id}`);
      chrome.closeDrawer();
    } else if (id === "theme.system" || id === "theme.dark" || id === "theme.light") {
      shell.theme.set(id.slice("theme.".length) as "system" | "dark" | "light");
    } else if (id === "sign-out") {
      void shell.signIn.signOut();
    }
  };

  return (
    <DropdownMenu
      label="Account"
      items={items()}
      minWidth="260px"
      placement="top-start"
      onSelect={choose}
      trigger={(trigger) => (
        <button {...trigger} type="button" class="nc-nav-item shell-account" data-part="account-menu">
          <span class="nc-nav-item__icon">
            <Avatar name={name()} size="sm" />
          </span>
          <span class="nc-nav-item__label">{name()}</span>
          <span class="nc-nav-item__meta" aria-hidden="true">
            <Icon name="chevron-down" size="sm" />
          </span>
        </button>
      )}
    />
  );
}

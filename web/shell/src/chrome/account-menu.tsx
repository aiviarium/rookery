// The person at the foot of the sidebar: who is signed in, their settings,
// Administration for an admin, the theme, and signing out. Nocturne's
// AccountMenu draws it.
import type { JSX } from "@solidjs/web";
import { AccountMenu as Menu, type MenuEntry } from "@aiviarium/nocturne";

import { useShell } from "../shell.tsx";
import { useChrome } from "./context.ts";

/** AccountMenu is the person's card, which opens their menu upwards. */
export function AccountMenu(): JSX.Element {
  const shell = useShell();
  const chrome = useChrome();

  const user = () => shell.signIn.user();
  const name = () => user()?.name || user()?.email || "Signed in";
  const items = (): MenuEntry[] => [
    { id: "settings", label: "Settings", icon: "settings" },
    ...(user()?.admin ? [{ id: "admin", label: "Administration", icon: "shield" as const }] : []),
  ];

  return (
    <Menu
      name={name()}
      detail={user()?.name ? user()?.email : undefined}
      badge={user()?.admin ? "Admin" : undefined}
      items={items()}
      onSelect={(id) => {
        shell.navigate(`/${id}`);
        chrome.closeDrawer();
      }}
      theme={{ value: shell.theme.choice(), onChange: (value) => shell.theme.set(value) }}
      onSignOut={() => void shell.signIn.signOut()}
    />
  );
}

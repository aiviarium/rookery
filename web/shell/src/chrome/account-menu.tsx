// Who is signed in, and how to stop being signed in.
import { Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Avatar, Badge, Button, DropdownMenu, Icon, type MenuEntry } from "@aiviarium/nocturne";

import { useShell } from "../shell.tsx";

/** AccountMenu is the person's own menu at the top right. */
export function AccountMenu(): JSX.Element {
  const shell = useShell();

  const user = () => shell.signIn.user();
  const name = () => user()?.name || user()?.email || "Account";

  const items = (): MenuEntry[] => [
    {
      type: "custom",
      content: (
        <div class="nc-stack" style={{ gap: "var(--space-1)", padding: "var(--space-2)" }}>
          <strong class="nc-truncate">{name()}</strong>
          <Show when={user()?.email !== undefined && user()?.email !== ""}>
            <span class="nc-meta nc-truncate">{user()?.email}</span>
          </Show>
          <Show when={user()?.admin === true}>
            <span>
              <Badge tone="accent">admin</Badge>
            </span>
          </Show>
        </div>
      ),
    },
    { type: "separator" },
    { id: "sign-out", label: "Sign out", icon: "arrow-up-right", danger: true },
  ];

  return (
    <DropdownMenu
      label="Account"
      placement="bottom-end"
      items={items()}
      onSelect={(id) => {
        if (id === "sign-out") void shell.signIn.signOut();
      }}
      trigger={trigger => (
        <Button {...trigger} variant="ghost" size="sm" aria-label="Account">
          <Avatar name={name()} size="sm" />
          <span class="nc-truncate">{name()}</span>
          <Icon name="chevron-down" size="sm" />
        </Button>
      )}
    />
  );
}

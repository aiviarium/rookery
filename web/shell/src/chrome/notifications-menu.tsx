// The bell: what happened while the person was not looking.
import { For, Show } from "solid-js";
import type { JSX } from "@solidjs/web";

import { useShell } from "../shell.tsx";
import { Badge, Button, Empty, Glyph, IconButton, Popover, relative, useToggle } from "../ui.tsx";

/** NotificationsMenu is the unread count and the notifications behind it. */
export function NotificationsMenu(): JSX.Element {
  const shell = useShell();
  const [open, toggle] = useToggle();

  return (
    <div style={relative}>
      <span style={{ position: "relative", display: "inline-flex" }}>
        <IconButton
          label={shell.notifications.unread() > 0 ? `Notifications, ${shell.notifications.unread()} unread` : "Notifications"}
          glyph="bell"
          expanded={open()}
          onClick={() => toggle()}
        />
        <Show when={shell.notifications.unread() > 0}>
          <span class="nc-badge nc-badge--accent" style={{ position: "absolute", top: "-4px", right: "-4px" }}>
            {shell.notifications.unread()}
          </span>
        </Show>
      </span>
      <Popover open={open()} onClose={() => toggle(false)} label="Notifications">
        <div class="nc-stack" style={{ "min-width": "320px", "max-width": "420px" }}>
          <div class="nc-row" style={{ "justify-content": "space-between", padding: "var(--space-2)" }}>
            <strong>Notifications</strong>
            <Show when={shell.notifications.unread() > 0}>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => {
                  void shell.notifications.markAllRead();
                }}
              >
                Mark all read
              </Button>
            </Show>
          </div>
          <Show
            when={shell.notifications.items().length > 0}
            fallback={<Empty title="Nothing yet" description="Rookery tells you here when a job fails or a roll-out pauses." />}
          >
            <ul class="nc-rows">
              <For each={shell.notifications.items()}>
                {notification => (
                  <li class="nc-lrow" data-lines="2">
                    <button
                      type="button"
                      class="nc-lrow__main"
                      style={{ "flex-direction": "column", "align-items": "flex-start", gap: "2px" }}
                      onClick={() => {
                        toggle(false);
                        void shell.notifications.markRead(notification.id, true);
                        if (notification.link !== "") shell.navigate(notification.link);
                      }}
                    >
                      <span class="nc-row" style={{ "justify-content": "space-between", width: "100%" }}>
                        <span class="nc-lrow-title">{notification.title}</span>
                        <Show when={!notification.read}>
                          <Badge tone="accent">new</Badge>
                        </Show>
                      </span>
                      <span class="nc-lrow-meta">{notification.body}</span>
                    </button>
                  </li>
                )}
              </For>
            </ul>
          </Show>
          <div class="nc-row" style={{ "justify-content": "flex-end", padding: "var(--space-1) var(--space-2)" }}>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => {
                void shell.notifications.refresh();
              }}
            >
              <Glyph name="search" size={14} /> Check again
            </Button>
          </div>
        </div>
      </Popover>
    </div>
  );
}

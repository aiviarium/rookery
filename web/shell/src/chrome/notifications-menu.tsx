// The bell: what happened while the person was not looking.
import { Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import {
  Button,
  Card,
  Count,
  EmptyState,
  IconButton,
  ListRow,
  Popover,
  RowList,
} from "@aiviarium/nocturne";
import type { Notification } from "@rookery/proto";

import { useShell } from "../shell.tsx";

/** NotificationsMenu is the unread count and the notifications behind it. */
export function NotificationsMenu(): JSX.Element {
  const shell = useShell();
  const [anchor, setAnchor] = createSignal<HTMLElement>();
  const [open, setOpen] = createSignal(false);

  const unread = () => shell.notifications.unread();
  const items = () => shell.notifications.items();

  const choose = (notification: Notification): void => {
    setOpen(false);
    void shell.notifications.markRead(notification.id, true);
    if (notification.link !== "") shell.navigate(notification.link);
  };

  return (
    <>
      <span class="shell-bell" ref={element => setAnchor(element)}>
        <IconButton
          label={unread() > 0 ? `Notifications, ${unread()} unread` : "Notifications"}
          icon="bell"
          pressed={open()}
          onClick={() => setOpen(!open())}
        />
        <Show when={unread() > 0}>
          <Count value={unread()} class="shell-bell__count" />
        </Show>
      </span>
      <Popover
        open={open()}
        anchor={anchor()}
        onClose={() => setOpen(false)}
        placement="bottom-end"
        aria-label="Notifications"
      >
        <Card
          title="Notifications"
          action={
            <Show when={unread() > 0}>
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
          }
          padding="none"
          footer={
            <Button
              variant="ghost"
              size="sm"
              icon="refresh-cw"
              onClick={() => {
                void shell.notifications.refresh();
              }}
            >
              Check again
            </Button>
          }
        >
          <div class="shell-notifications">
            <Show
              when={items().length > 0}
              fallback={
                <EmptyState
                  size="sm"
                  title="Nothing yet"
                  description="Rookery tells you here when a job fails or a roll-out pauses."
                />
              }
            >
              <RowList label="Notifications" variant="inset">
                {items().map(notification => (
                  <ListRow
                    title={notification.title}
                    secondary={notification.body}
                    lines={2}
                    unread={!notification.read}
                    onClick={() => choose(notification)}
                  />
                ))}
              </RowList>
            </Show>
          </div>
        </Card>
      </Popover>
    </>
  );
}

// The inbox: what happened while the person was not looking. A notification
// opens where it points and is marked read; its menu marks it read or unread.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import type { Notification } from "@rookery/proto";
import { Callout, Card, EmptyState, ListRow, type MenuEntry, RowList, Tabs } from "@aiviarium/nocturne";

import { createAction } from "../../parts/action.ts";
import { errorMessage } from "../../parts/errors.ts";
import { RowActions } from "../../parts/row-actions.tsx";
import { View } from "../../parts/view.tsx";
import { useShell } from "../../shell.tsx";
import { dateOf, relativeTime } from "../fleet/format.ts";

/** InboxPage is the person's notifications, newest first. */
export function InboxPage(): JSX.Element {
  const shell = useShell();
  const [filter, setFilter] = createSignal<"all" | "unread">("all");
  const items = () => shell.notifications.items().filter((item) => filter() === "all" || !item.read);
  const unread = () => shell.notifications.unread();

  const markAll = createAction(() => shell.notifications.markAllRead());
  const mark = createAction((id: string, read: boolean) => shell.notifications.markRead(id, read), {
    failed: "The notification was not changed",
  });

  const open = (notification: Notification) => {
    if (!notification.read) void mark.run(notification.id, true);
    if (notification.link !== "") shell.navigate(notification.link);
  };
  const menu = (notification: Notification): MenuEntry[] => [
    notification.read ? { id: "unread", label: "Mark unread", icon: "eye" } : { id: "read", label: "Mark read", icon: "check" },
    ...(notification.link === "" ? [] : [{ id: "open", label: "Open", icon: "arrow-right" } as MenuEntry]),
  ];
  const choose = (notification: Notification, id: string) => {
    if (id === "open") open(notification);
    else void mark.run(notification.id, id === "read");
  };

  return (
    <View
      id="inbox"
      crumbs={[{ label: "Inbox" }]}
      actions={[
        ...(unread() > 0 ? [{ id: "read", label: "Mark all read", icon: "check" as const, disabled: markAll.busy(), onClick: () => void markAll.run() }] : []),
        { id: "refresh", label: "Check again", icon: "refresh-cw", overflow: true, onClick: () => void shell.notifications.refresh() },
      ]}
    >
      <div class="shell-fit">
        <Tabs
          label="Show"
          value={filter()}
          onChange={(next) => setFilter(next as "all" | "unread")}
          items={[
            { id: "all", label: "All" },
            { id: "unread", label: "Unread", count: unread() > 0 ? unread() : undefined },
          ]}
        />
      </div>
      <Show when={shell.notifications.error()}>
        {(failure) => <Callout tone="danger">The notifications could not be read. {errorMessage(failure())}</Callout>}
      </Show>
      <Card padding="none">
        <Show
          when={items().length > 0}
          fallback={
            <EmptyState icon="inbox" title={filter() === "unread" ? "Nothing unread" : "Nothing yet"} />
          }
        >
          <RowList label="Notifications">
            <For each={items()} keyed={(item) => item.id}>
              {(notification) => (
                <ListRow
                  icon="bell"
                  title={notification().title}
                  secondary={notification().body}
                  lines={2}
                  unread={!notification().read}
                  meta={relativeTime(dateOf(notification().createdAt))}
                  onClick={() => open(notification())}
                  menu={menu(notification())}
                  onMenuSelect={(id) => choose(notification(), id)}
                  persistentActions={
                    <RowActions label={`Actions for ${notification().title}`} items={menu(notification())} onSelect={(id) => choose(notification(), id)} />
                  }
                />
              )}
            </For>
          </RowList>
        </Show>
      </Card>
    </View>
  );
}

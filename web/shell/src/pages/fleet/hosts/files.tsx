// A host's files, as one of its accounts: browse a directory, download a file,
// upload one, and delete what is there.
//
// A path is the account's: "" is its home, and a relative path is under it.
// Uploading replaces a file of the same name. The directory is in the address,
// so a reload or a link lands on the same one.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useSearchParams } from "@solidjs/router";
import type { HostFile } from "@rookery/proto";
import { Callout, Card, EmptyState, IconButton, ListRow, RowList, Skeleton, Tabs } from "@aiviarium/nocturne";

import { createAction } from "../../../parts/action.ts";
import { createLive } from "../../../parts/data.ts";
import { ConfirmDialog } from "../../../parts/dialogs.tsx";
import { RowActions } from "../../../parts/row-actions.tsx";
import { View } from "../../../parts/view.tsx";
import { useShell } from "../../../shell.tsx";
import { bytesSize, dateOf, relativeTime } from "../format.ts";
import { useHost } from "./domain.tsx";

type Account = "operator" | "desktop" | "root";

const ACCOUNTS: ReadonlyArray<{ value: Account; label: string }> = [
  { value: "operator", label: "Operator" },
  { value: "desktop", label: "Desktop" },
  { value: "root", label: "Root" },
];

/** parentOf is the directory above a path, "" for the home directory. */
export function parentOf(path: string): string {
  if (path === "" || path === "/") return "";
  const absolute = path.startsWith("/");
  const parts = path.split("/").filter((part) => part !== "");
  parts.pop();
  if (parts.length === 0) return absolute ? "/" : "";
  return `${absolute ? "/" : ""}${parts.join("/")}`;
}

/** segmentsOf are the directories down to a path, for its crumbs. */
export function segmentsOf(path: string): Array<{ label: string; path: string }> {
  const absolute = path.startsWith("/");
  const parts = path.split("/").filter((part) => part !== "");
  const segments = [{ label: absolute ? "/" : "~", path: absolute ? "/" : "" }];
  parts.forEach((part, index) => {
    const joined = parts.slice(0, index + 1).join("/");
    segments.push({ label: part, path: absolute ? `/${joined}` : joined });
  });
  return segments;
}

/** HostFilesPage is a host's files place. */
export function HostFilesPage(): JSX.Element {
  const shell = useShell();
  const domain = useHost();
  const [search, setSearch] = useSearchParams<{ path?: string; as?: string }>();
  const path = () => search.path ?? "";
  const account = (): Account => (ACCOUNTS.some((one) => one.value === search.as) ? (search.as as Account) : "operator");
  const [removing, setRemoving] = createSignal<HostFile>();
  let upload: HTMLInputElement | undefined;

  const listing = createLive(
    () => shell.rookery.files.list({ host: domain.name(), file: { user: { name: account() }, path: path() } }),
    { key: () => `${domain.name()}\n${account()}\n${path()}` },
  );
  const entries = () =>
    [...(listing()?.result?.entries ?? [])].sort((a, b) => (a.dir === b.dir ? a.name.localeCompare(b.name) : a.dir ? -1 : 1));
  const go = (next: string) => setSearch({ path: next === "" ? undefined : next });
  const refresh = () => void listing.refresh().catch(() => undefined);

  const download = createAction(
    async (entry: HostFile) => {
      const read = await shell.rookery.files.read({ host: domain.name(), file: { user: { name: account() }, path: entry.path } });
      const data = read.result?.data ?? new Uint8Array();
      const url = URL.createObjectURL(new Blob([data.slice()], { type: "application/octet-stream" }));
      const anchor = document.createElement("a");
      anchor.href = url;
      anchor.download = entry.name;
      anchor.click();
      URL.revokeObjectURL(url);
    },
    { failed: "The file did not download" },
  );

  const send = createAction(
    async (file: File) => {
      const data = new Uint8Array(await file.arrayBuffer());
      const target = path() === "" ? file.name : `${path().replace(/\/$/, "")}/${file.name}`;
      await shell.rookery.files.write({
        host: domain.name(),
        file: { user: { name: account() }, path: target, data, create: false },
      });
      refresh();
    },
    { done: "Uploaded", failed: "The file did not upload" },
  );

  const remove = createAction(async () => {
    const entry = removing();
    if (entry === undefined) return;
    await shell.rookery.files.delete({
      host: domain.name(),
      file: { user: { name: account() }, path: entry.path, recursive: entry.dir },
    });
    refresh();
  });

  return (
    <View
      id="host-files"
      crumbs={domain.crumbs("Files")}
      bar={
        <div class="shell-toolbar">
          <Tabs
            label="Account"
            idPrefix="files-account"
            items={ACCOUNTS.map((one) => ({ id: one.value, label: one.label }))}
            value={account()}
            onChange={(chosen) => setSearch({ as: chosen === "operator" ? undefined : chosen, path: undefined })}
          />
          <IconButton
            size="sm"
            icon="chevron-left"
            label="Up a directory"
            data-part="files-up"
            disabled={path() === ""}
            onClick={() => go(parentOf(path()))}
          />
          <nav class="shell-path nc-mono" aria-label="Directory">
            <For each={segmentsOf(path())}>
              {(segment, index) => (
                <>
                  <Show when={index() > 0}>
                    <span class="shell-path__sep" aria-hidden="true">
                      /
                    </span>
                  </Show>
                  <button type="button" class="shell-path__part" onClick={() => go(segment.path)}>
                    {segment.label}
                  </button>
                </>
              )}
            </For>
          </nav>
        </div>
      }
      actions={[
        { id: "upload", label: send.busy() ? "Uploading…" : "Upload", icon: "upload", disabled: send.busy(), onClick: () => upload?.click() },
        { id: "refresh", label: "Read the directory again", icon: "refresh-cw", overflow: true, onClick: refresh },
      ]}
    >
      <input
        type="file"
        hidden
        ref={(element) => {
          upload = element ?? undefined;
        }}
        onChange={(event) => {
          const file = event.currentTarget.files?.[0];
          if (file !== undefined) void send.run(file);
          event.currentTarget.value = "";
        }}
      />
      <Show when={listing.error()}>
        {(error) => <Callout tone="danger">{error().message}</Callout>}
      </Show>
      <Card padding="none">
        <Show when={listing() !== undefined} fallback={<div class="shell-pad"><Skeleton width="30%" height="14px" /></div>}>
          <Show
            when={entries().length > 0}
            fallback={<EmptyState size="sm" icon="folder" title="Empty directory" />}
          >
            <RowList label={`Files in ${path() === "" ? "the home directory" : path()}`}>
              <For each={entries()}>
                {(entry) => (
                  <ListRow
                    data-file={entry.path}
                    icon={entry.dir ? "folder" : "file"}
                    title={<span class="nc-mono">{entry.dir ? `${entry.name}/` : entry.name}</span>}
                    detail={entry.mode === 0 ? undefined : <span class="nc-mono">0{entry.mode.toString(8)}</span>}
                    meta={
                      <span class="nc-meta">
                        {entry.dir ? "" : `${bytesSize(Number(entry.size))} · `}
                        {relativeTime(dateOf(entry.modifiedAt))}
                      </span>
                    }
                    onClick={entry.dir ? () => go(entry.path) : () => void download.run(entry)}
                    menu={[
                      ...(entry.dir ? [] : [{ id: "download", label: "Download", icon: "download" as const }]),
                      { id: "delete", label: "Delete", icon: "trash" as const, danger: true },
                    ]}
                    menuLabel={`Actions for ${entry.name}`}
                    onMenuSelect={(id) => (id === "download" ? void download.run(entry) : setRemoving(entry))}
                    persistentActions={
                      <RowActions
                        label={`Actions for ${entry.name}`}
                        items={[
                          ...(entry.dir ? [] : [{ id: "download", label: "Download", icon: "download" as const }]),
                          { type: "separator" as const },
                          { id: "delete", label: "Delete", icon: "trash" as const, danger: true },
                        ]}
                        onSelect={(id) => (id === "download" ? void download.run(entry) : setRemoving(entry))}
                      />
                    }
                  />
                )}
              </For>
            </RowList>
          </Show>
        </Show>
      </Card>
      <ConfirmDialog
        open={removing() !== undefined}
        title={`Delete ${removing()?.name ?? ""}?`}
        description={removing()?.dir ? "With everything in it." : undefined}
        confirm="Delete"
        danger
        action={remove}
        onClose={() => setRemoving(undefined)}
      />
    </View>
  );
}

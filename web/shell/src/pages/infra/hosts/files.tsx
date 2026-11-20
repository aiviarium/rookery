// A host's files, as one of its accounts: browse a directory, download a file,
// upload one, and delete what is there.
//
// A path is the account's: "" is its home, and a relative path is under it.
// Uploading replaces a file of the same name.
import { Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import type { HostFile } from "@rookery/proto";
import { Button, Callout, Card, DataTable, type DataColumn, Select, TextField } from "@aiviarium/nocturne";

import { useShell } from "../../../shell.tsx";
import { Spinner } from "../../../parts/spinner.tsx";
import { bytesSize, errorMessage, relativeTime, dateOf } from "../format.ts";

type Account = "operator" | "desktop" | "root";

export function HostFiles(props: { host: string }): JSX.Element {
  const shell = useShell();
  const [account, setAccount] = createSignal<Account>("operator");
  const [path, setPath] = createSignal("");
  const [draft, setDraft] = createSignal("");
  const [entries, setEntries] = createSignal<HostFile[]>([]);
  const [loaded, setLoaded] = createSignal(false);
  const [busy, setBusy] = createSignal<string>();
  const [failure, setFailure] = createSignal<string>();
  let upload: HTMLInputElement | undefined;

  const open = async (next: string): Promise<void> => {
    setBusy("list");
    setFailure(undefined);
    try {
      const listed = await shell.rookery.files.list({
        host: props.host,
        file: { user: { name: account() }, path: next },
      });
      setEntries(listed.result?.entries ?? []);
      setPath(next);
      setDraft(next);
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy(undefined);
      setLoaded(true);
    }
  };

  const parent = (): string => {
    const current = path();
    if (current === "" || current === "/") return "";
    const parts = current.split("/").filter(part => part !== "");
    parts.pop();
    const joined = parts.join("/");
    return joined === "" ? "/" : `${current.startsWith("/") ? "/" : ""}${joined}`;
  };

  const download = async (entry: HostFile): Promise<void> => {
    setBusy(entry.path);
    setFailure(undefined);
    try {
      const read = await shell.rookery.files.read({
        host: props.host,
        file: { user: { name: account() }, path: entry.path },
      });
      const data = read.result?.data ?? new Uint8Array();
      const url = URL.createObjectURL(new Blob([data.slice()], { type: "application/octet-stream" }));
      const anchor = document.createElement("a");
      anchor.href = url;
      anchor.download = entry.name;
      anchor.click();
      URL.revokeObjectURL(url);
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy(undefined);
    }
  };

  const send = async (file: File): Promise<void> => {
    setBusy(file.name);
    setFailure(undefined);
    try {
      const data = new Uint8Array(await file.arrayBuffer());
      const target = path() === "" ? file.name : `${path().replace(/\/$/, "")}/${file.name}`;
      await shell.rookery.files.write({
        host: props.host,
        file: { user: { name: account() }, path: target, data, create: false },
      });
      await open(path());
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy(undefined);
    }
  };

  const remove = async (entry: HostFile): Promise<void> => {
    setBusy(entry.path);
    setFailure(undefined);
    try {
      await shell.rookery.files.delete({
        host: props.host,
        file: { user: { name: account() }, path: entry.path, recursive: entry.dir },
      });
      await open(path());
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy(undefined);
    }
  };

  const columns = (): DataColumn<HostFile>[] => [
    {
      id: "name",
      header: "Name",
      type: "mono",
      accessor: entry => (entry.dir ? `${entry.name}/` : entry.name),
    },
    { id: "size", header: "Size", type: "num", accessor: entry => (entry.dir ? "" : bytesSize(Number(entry.size))) },
    {
      id: "mode",
      header: "Mode",
      type: "mono",
      accessor: entry => (entry.mode === 0 ? "" : `0${entry.mode.toString(8)}`),
    },
    { id: "modified", header: "Modified", type: "time", accessor: entry => relativeTime(dateOf(entry.modifiedAt)) },
    {
      id: "actions",
      header: "",
      cell: entry => (
        <span class="nc-row nc-row--tight">
          <Button size="sm" disabled={entry.dir} onClick={() => void download(entry)}>
            Download
          </Button>
          <Button size="sm" variant="danger" onClick={() => void remove(entry)}>
            Delete
          </Button>
        </span>
      ),
    },
  ];

  return (
    <Card title="Files" padding="none">
      <div class="shell-pad nc-stack">
        <form
          class="nc-row"
          onSubmit={event => {
            event.preventDefault();
            void open(draft());
          }}
        >
          <Select
            aria-label="Account"
            value={account()}
            options={[
              { value: "operator", label: "operator" },
              { value: "desktop", label: "desktop" },
              { value: "root", label: "root" },
            ]}
            onChange={chosen => {
              setAccount(chosen as Account);
              void open("");
            }}
          />
          <TextField
            label="Path"
            value={draft()}
            placeholder="~ (the account's home)"
            onInput={event => setDraft(event.currentTarget.value)}
          />
          <Button type="submit" size="sm">
            Go
          </Button>
          <Button size="sm" onClick={() => void open(parent())}>
            Up
          </Button>
          <Button size="sm" onClick={() => upload?.click()}>
            Upload
          </Button>
          <Button size="sm" onClick={() => void open(path())}>
            Refresh
          </Button>
          <input
            type="file"
            hidden
            ref={element => {
              upload = element ?? undefined;
            }}
            onChange={event => {
              const file = event.currentTarget.files?.[0];
              if (file !== undefined) void send(file);
              event.currentTarget.value = "";
            }}
          />
        </form>
        <Show when={path() !== ""}>
          <span class="nc-meta nc-code">{path()}</span>
        </Show>
        <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
        <Show when={busy() !== undefined}>
          <span class="nc-row nc-row--tight">
            <Spinner label={busy()} /> {busy()}
          </span>
        </Show>
        <Show
          when={loaded()}
          fallback={
            <Button size="sm" onClick={() => void open("")}>
              Read the home directory
            </Button>
          }
        >
          <DataTable
            bare
            label={`Files in ${path() === "" ? "the home directory" : path()}`}
            rows={entries()}
            columns={columns()}
            getRowId={entry => entry.path}
            onRowClick={entry => {
              if (entry.dir) void open(entry.path);
            }}
            empty={{ title: "This directory has no files." }}
          />
        </Show>
      </div>
    </Card>
  );
}

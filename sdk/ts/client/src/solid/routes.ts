// The routes that are not Connect, as Solid signals: a live document's updates,
// a terminal's output and exit, and a host's desktop socket.
//
// Each helper opens its stream with its owner and closes it when the owner is
// disposed, so a component that unmounts leaves nothing running.
import { type Accessor, createSignal } from "solid-js";

import { DocumentSync, type DocumentOptions } from "../documents.ts";
import { DesktopStream, type DesktopOptions } from "../desktop.ts";
import type { RookeryEndpoints } from "../endpoints.ts";
import type { SocketStatus } from "../socket.ts";
import { TerminalSession, type TerminalOptions } from "../terminals.ts";
import { fromWatchable, onDispose } from "./signals.ts";

/** DocumentHandle is a live document, as a component reads it. */
export interface DocumentHandle {
  /** document is the socket, for what this handle does not cover. */
  readonly document: DocumentSync;
  /** status is where the socket is. */
  status: Accessor<SocketStatus>;
  /** update is the last update from the server, which a CRDT applies. */
  update: Accessor<Uint8Array | undefined>;
  /** awareness is the last other participant's state. */
  awareness: Accessor<Uint8Array | undefined>;
  /** sendUpdate sends one CRDT update. */
  sendUpdate(update: Uint8Array): void;
  /** sendAwareness sends this participant's state. */
  sendAwareness(state: Uint8Array): void;
  /** close stops syncing before the owner is disposed. */
  close(): void;
}

/** useDocument opens a document for as long as its owner lives. */
export function useDocument(
  endpoints: RookeryEndpoints,
  id: string,
  options: DocumentOptions = {},
): DocumentHandle {
  const document = new DocumentSync(endpoints, id, options);
  const [update, setUpdate] = createSignal<Uint8Array>();
  const [awareness, setAwareness] = createSignal<Uint8Array>();
  document.onUpdate(setUpdate);
  document.onAwareness(setAwareness);
  document.open();
  onDispose(() => document.close());

  return {
    document,
    status: fromWatchable(document.status),
    update,
    awareness,
    sendUpdate: (bytes) => document.sendUpdate(bytes),
    sendAwareness: (state) => document.sendAwareness(state),
    close: () => document.close(),
  };
}

/** TerminalHandle is an attached terminal, as a component reads it. */
export interface TerminalHandle {
  /** terminal is the socket, for what this handle does not cover. */
  readonly terminal: TerminalSession;
  /** status is where the socket is. */
  status: Accessor<SocketStatus>;
  /** output is everything the terminal printed, as UTF-8 text. */
  output: Accessor<string>;
  /** exit is the command's exit code, once it ended. */
  exit: Accessor<number | undefined>;
  /** write sends keystrokes. */
  write(data: string | Uint8Array): void;
  /** resize asks the PTY to change size. */
  resize(cols: number, rows: number): void;
  /** close detaches. */
  close(): void;
}

/** useTerminal attaches a terminal for as long as its owner lives. */
export function useTerminal(
  endpoints: RookeryEndpoints,
  id: string,
  options: TerminalOptions = {},
): TerminalHandle {
  const terminal = new TerminalSession(endpoints, id, options);
  const [output, setOutput] = createSignal("");
  const [exit, setExit] = createSignal<number>();
  const decoder = new TextDecoder();
  terminal.onData((data) => setOutput((text) => text + decoder.decode(data, { stream: true })));
  terminal.onExit(setExit);
  terminal.open();
  onDispose(() => terminal.close());

  return {
    terminal,
    status: fromWatchable(terminal.status),
    output,
    exit,
    write: (data) => terminal.write(data),
    resize: (cols, rows) => terminal.resize(cols, rows),
    close: () => terminal.close(),
  };
}

/** DesktopHandle is a host's desktop, as a component reads it. */
export interface DesktopHandle {
  /** desktop is the RFB socket, which an RFB client writes its handshake to. */
  readonly desktop: DesktopStream;
  /** readyState is the socket's: 0 connecting, 1 open, 2 closing, 3 closed. */
  readyState: Accessor<number>;
  /** send writes RFB bytes to the host. */
  send(data: Uint8Array): void;
  /** close ends the stream. */
  close(): void;
}

/**
 * useDesktop opens a host's desktop for as long as its owner lives. An RFB client
 * that takes a socket writes to `desktop`; one that opens its own WebSocket takes
 * `desktopUrl(rookery, host)` instead.
 */
export function useDesktop(
  endpoints: RookeryEndpoints,
  host: string,
  options: DesktopOptions = {},
): DesktopHandle {
  const desktop = new DesktopStream(endpoints, host, options);
  const [readyState, setReadyState] = createSignal(desktop.readyState);
  const update = () => setReadyState(desktop.readyState);
  const listener = update as unknown as (event: never) => void;
  desktop.addEventListener("open", listener);
  desktop.addEventListener("close", listener);
  desktop.addEventListener("error", listener);
  desktop.open();
  onDispose(() => desktop.close());

  return {
    desktop,
    readyState,
    send: (data) => desktop.send(data),
    close: () => desktop.close(),
  };
}

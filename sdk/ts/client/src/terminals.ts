// A terminal on a host, attached from the browser over `/ws/terminals/{id}`.
//
// Binary frames are the PTY's bytes in both directions, unread by this client.
// Text frames are JSON control messages: `{"type":"resize","cols":…,"rows":…}`
// to the server, and `{"type":"exit","code":…}` from it.
import type { RookeryEndpoints, SocketRouteOptions } from "./endpoints.ts";
import { type SocketStatus, Wire } from "./socket.ts";
import type { Watchable } from "./watchable.ts";

/** TerminalOptions configures one terminal's route. */
export interface TerminalOptions extends SocketRouteOptions {
  /** onError hears a socket failure. */
  onError?: (error: Error) => void;
}

/** TerminalControl is what a text frame of the terminal route carries. */
export type TerminalControl =
  | { readonly type: "resize"; readonly cols: number; readonly rows: number }
  | { readonly type: "exit"; readonly code: number };

/** TerminalSession is one attached terminal. */
export class TerminalSession {
  /** id is the terminal's id. */
  readonly id: string;
  readonly #wire: Wire;
  readonly #encoder = new TextEncoder();
  readonly #data = new Set<(data: Uint8Array) => void>();
  readonly #exit = new Set<(code: number) => void>();

  constructor(endpoints: RookeryEndpoints, id: string, options: TerminalOptions = {}) {
    this.id = id;
    this.#wire = new Wire({
      url: endpoints.webSocketUrl(`/ws/terminals/${encodeURIComponent(id)}`),
      factory: options.factory,
      onError: options.onError,
      onBinary: (bytes) => {
        for (const listener of [...this.#data]) listener(bytes);
      },
      onText: (text) => {
        const control = parseControl(text);
        if (control?.type !== "exit") return;
        for (const listener of [...this.#exit]) listener(control.code);
      },
    });
  }

  /** status is where the socket is. */
  get status(): Watchable<SocketStatus> {
    return this.#wire.status;
  }

  /** open attaches. */
  open(): this {
    this.#wire.open();
    return this;
  }

  /** close detaches; the terminal itself keeps running until it is closed. */
  close(): void {
    this.#wire.close();
    this.#data.clear();
    this.#exit.clear();
  }

  /** write sends keystrokes: bytes, or text as UTF-8. */
  write(data: string | Uint8Array): void {
    this.#wire.send(typeof data === "string" ? this.#encoder.encode(data) : data);
  }

  /** resize asks the PTY to change size. */
  resize(cols: number, rows: number): void {
    this.#wire.send(JSON.stringify({ type: "resize", cols, rows }));
  }

  /** onData calls listener for each chunk of output. */
  onData(listener: (data: Uint8Array) => void): () => void {
    this.#data.add(listener);
    return () => {
      this.#data.delete(listener);
    };
  }

  /** onExit calls listener when the terminal's command ended. */
  onExit(listener: (code: number) => void): () => void {
    this.#exit.add(listener);
    return () => {
      this.#exit.delete(listener);
    };
  }
}

/** parseControl reads a text frame, and undefined for a message it does not know. */
function parseControl(text: string): TerminalControl | undefined {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    return undefined;
  }
  if (typeof value !== "object" || value === null) return undefined;
  const control = value as Record<string, unknown>;
  if (control["type"] === "resize" && typeof control["cols"] === "number" && typeof control["rows"] === "number") {
    return { type: "resize", cols: control["cols"], rows: control["rows"] };
  }
  if (control["type"] === "exit" && typeof control["code"] === "number") {
    return { type: "exit", code: control["code"] };
  }
  return undefined;
}

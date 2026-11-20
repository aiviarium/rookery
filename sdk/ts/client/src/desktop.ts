// A host's desktop as RFB, over `/ws/hosts/{name}/desktop`.
//
// RFB is a byte stream, and the route passes it over the WebSocket unread: binary
// frames are RFB bytes in both directions. A shell shows it with noVNC, which
// takes a URL to open itself (`desktopUrl`), or with a client of its own that
// takes a socket (`openDesktop`), which is a WebSocket as far as such a client
// can tell.
import type { RookeryEndpoints, SocketRouteOptions } from "./endpoints.ts";
import { type SocketFactory, Wire } from "./socket.ts";
import { type Watchable, watchable } from "./watchable.ts";

/** desktopUrl is the RFB route of a host's desktop. */
export function desktopUrl(endpoints: RookeryEndpoints, host: string): string {
  return endpoints.webSocketUrl(`/ws/hosts/${encodeURIComponent(host)}/desktop`);
}

/** DesktopOptions configures one desktop stream. */
export interface DesktopOptions extends SocketRouteOptions {
  /** onError hears a socket failure. */
  onError?: (error: Error) => void;
}

/**
 * DesktopStream is a host's desktop, as a socket an RFB client can consume: the
 * WebSocket interface, with RFB bytes in `MessageEvent.data` as an ArrayBuffer.
 */
export class DesktopStream {
  /** url is the route's address. */
  readonly url: string;
  /** binaryType is always "arraybuffer", which an RFB client expects. */
  binaryType: string = "arraybuffer";

  readonly #status: Watchable<"connecting" | "open" | "closing" | "closed">;
  readonly #wire: Wire;
  #listeners = new Map<string, Set<(event: unknown) => void>>();

  /** onopen is called once the stream is ready for RFB's server greeting. */
  onopen: ((event: unknown) => void) | null = null;
  /** onclose is called when the stream ended. */
  onclose: ((event: unknown) => void) | null = null;
  /** onerror is called when the stream failed. */
  onerror: ((event: unknown) => void) | null = null;
  /** onmessage is called with each chunk of RFB bytes. */
  onmessage: ((event: { data: ArrayBuffer }) => void) | null = null;

  constructor(endpoints: RookeryEndpoints, host: string, options: DesktopOptions = {}) {
    this.url = desktopUrl(endpoints, host);
    this.#status = watchable("connecting");
    this.#wire = new Wire({
      url: this.url,
      factory: options.factory,
      onError: options.onError,
      onStatus: (status) => {
        switch (status) {
          case "open":
            this.#status.set("open");
            this.#dispatch("open", {});
            return;
          case "closed":
            this.#status.set("closed");
            this.#dispatch("close", { code: 1000 });
            return;
          case "error":
            this.#dispatch("error", {});
            return;
          default:
            return;
        }
      },
      onBinary: (bytes) => {
        const data = bytes.slice().buffer;
        this.#dispatch("message", { data });
      },
    });
  }

  /** readyState is 0 connecting, 1 open, 2 closing or 3 closed. */
  get readyState(): number {
    switch (this.#status.get()) {
      case "open":
        return 1;
      case "closing":
        return 2;
      case "closed":
        return 3;
      default:
        return 0;
    }
  }

  /** open connects. */
  open(): this {
    this.#wire.open();
    return this;
  }

  /** send writes RFB bytes to the host. */
  send(data: string | ArrayBufferLike | ArrayBufferView): void {
    if (typeof data === "string") {
      this.#wire.send(new TextEncoder().encode(data));
      return;
    }
    this.#wire.send(
      ArrayBuffer.isView(data)
        ? new Uint8Array(data.buffer as ArrayBuffer, data.byteOffset, data.byteLength)
        : new Uint8Array(data),
    );
  }

  /** close ends the stream. */
  close(_code?: number, _reason?: string): void {
    this.#status.set("closing");
    this.#wire.close();
  }

  /** addEventListener adds a listener, as a WebSocket's does. */
  addEventListener(type: string, listener: (event: never) => void): void {
    let set = this.#listeners.get(type);
    if (set === undefined) {
      set = new Set();
      this.#listeners.set(type, set);
    }
    set.add(listener as (event: unknown) => void);
  }

  /** removeEventListener removes one. */
  removeEventListener(type: string, listener: (event: never) => void): void {
    this.#listeners.get(type)?.delete(listener as (event: unknown) => void);
  }

  /** #dispatch tells the property and the listeners of an event. */
  #dispatch(type: string, event: unknown): void {
    const property = this[`on${type}` as "onopen" | "onclose" | "onerror" | "onmessage"];
    if (typeof property === "function") (property as (event: unknown) => void)(event);
    for (const listener of [...(this.#listeners.get(type) ?? [])]) listener(event);
  }
}

/** openDesktop opens a host's desktop, and returns the socket before it is open. */
export function openDesktop(
  endpoints: RookeryEndpoints,
  host: string,
  options: DesktopOptions = {},
): DesktopStream {
  return new DesktopStream(endpoints, host, options).open();
}

/** openDesktopWith opens a desktop through one socket factory, which tests use. */
export function openDesktopWith(
  factory: SocketFactory,
  endpoints: RookeryEndpoints,
  host: string,
): DesktopStream {
  return new DesktopStream(endpoints, host, { factory }).open();
}

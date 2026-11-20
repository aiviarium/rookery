// The WebSocket routes of the edge, as the browser uses them: a socket, its
// status, and the two kinds of frame it carries. `/ws/documents/{id}`, 
// `/ws/terminals/{id}` and `/ws/hosts/{name}/desktop` all build on this.
import { type Watchable, watchable } from "./watchable.ts";

/** SocketStatus is where a socket is. */
export type SocketStatus = "connecting" | "open" | "closed" | "error";

/**
 * SocketLike is the part of a WebSocket this client uses, so that a test, or a
 * host without one, can supply its own. A browser's WebSocket is one.
 */
export interface SocketLike {
  binaryType: string;
  readonly readyState: number;
  send(data: ArrayBufferLike | ArrayBufferView | string): void;
  close(code?: number, reason?: string): void;
  addEventListener(type: string, listener: (event: never) => void): void;
  removeEventListener(type: string, listener: (event: never) => void): void;
}

/** SocketFactory opens a socket to a URL. */
export type SocketFactory = (url: string) => SocketLike;

/** browserSocket is the platform's WebSocket. */
export const browserSocket: SocketFactory = (url) =>
  new WebSocket(url) as unknown as SocketLike;

/** WireFrame is one frame of a route's binary protocol: its kind and its bytes. */
export interface WireFrame {
  /** kind is the frame's first byte, which the route's protocol names. */
  readonly kind: number;
  /** payload is the rest of the frame. */
  readonly payload: Uint8Array;
}

/** WireOptions configures one socket route. */
export interface WireOptions {
  /** url is the ws:// or wss:// address. */
  url: string;
  /** factory opens the socket; it defaults to the browser's WebSocket. */
  factory?: SocketFactory;
  /** binary is what a binary frame's bytes mean. */
  onBinary?: (payload: Uint8Array) => void;
  /** framed reads binary frames as a kind byte and a payload. */
  onFrame?: (frame: WireFrame) => void;
  /** text is a text frame: a JSON control message. */
  onText?: (text: string) => void;
  /** onStatus hears every change of status. */
  onStatus?: (status: SocketStatus) => void;
  /** onError hears a socket error, and then the close that follows it. */
  onError?: (error: Error) => void;
}

/**
 * Wire is one WebSocket route: open, send, hear frames, close. It does not
 * reconnect by itself; the routes that need it say so.
 */
export class Wire {
  readonly #options: WireOptions;
  readonly #status = watchable<SocketStatus>("connecting");
  #socket: SocketLike | undefined;

  #onOpen = (): void => this.#setStatus("open");
  #onClose = (): void => this.#setStatus("closed");
  #onError = (): void => {
    this.#options.onError?.(new Error(`the socket to ${this.#options.url} failed`));
    this.#setStatus("error");
  };
  #onMessage = (event: MessageEvent): void => {
    const data: unknown = event.data;
    if (typeof data === "string") {
      this.#options.onText?.(data);
      return;
    }
    if (typeof Blob !== "undefined" && data instanceof Blob) {
      // A socket may hand a browser a Blob instead of bytes; both mean the same.
      void data.arrayBuffer().then((buffer) => this.#binary(new Uint8Array(buffer)));
      return;
    }
    const bytes = toBytes(data);
    if (bytes !== undefined) this.#binary(bytes);
  };

  /** #binary hands one binary frame to the route. */
  #binary(bytes: Uint8Array): void {
    if (this.#options.onFrame !== undefined) {
      if (bytes.length === 0) return;
      this.#options.onFrame({
        kind: bytes[0] as number,
        payload: bytes.subarray(1),
      });
      return;
    }
    this.#options.onBinary?.(bytes);
  }

  constructor(options: WireOptions) {
    this.#options = options;
  }

  /** status is the socket's status and its listeners. */
  get status(): Watchable<SocketStatus> {
    return this.#status;
  }

  /** status is the status now. */
  get(): SocketStatus {
    return this.#status.get();
  }

  /** open connects. A closed wire stays closed. */
  open(): void {
    if (this.#socket !== undefined) return;
    const socket = (this.#options.factory ?? browserSocket)(this.#options.url);
    socket.binaryType = "arraybuffer";
    socket.addEventListener("open", this.#onOpen);
    socket.addEventListener("close", this.#onClose);
    socket.addEventListener("error", this.#onError);
    socket.addEventListener("message", this.#onMessage as (event: never) => void);
    this.#socket = socket;
  }

  /** send writes bytes, once the socket is open. Bytes before that are dropped. */
  send(data: Uint8Array | string): void {
    if (this.#socket === undefined || this.#socket.readyState !== 1) return;
    if (typeof data === "string") {
      this.#socket.send(data);
      return;
    }
    // A fresh copy: a view of a socket's own buffer must not travel.
    this.#socket.send(data.slice().buffer);
  }

  /** close closes the socket and tells the listeners. */
  close(): void {
    const socket = this.#socket;
    this.#socket = undefined;
    if (socket !== undefined) {
      socket.removeEventListener("open", this.#onOpen);
      socket.removeEventListener("close", this.#onClose);
      socket.removeEventListener("error", this.#onError);
      socket.removeEventListener("message", this.#onMessage as (event: never) => void);
      socket.close();
    }
    this.#setStatus("closed");
  }

  /** #setStatus records and announces a status. */
  #setStatus(status: SocketStatus): void {
    if (this.#status.get() === status) return;
    this.#status.set(status);
    this.#options.onStatus?.(status);
  }
}

/** toBytes reads a message event's data as bytes, whichever form it came in. */
export function toBytes(data: unknown): Uint8Array | undefined {
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  if (ArrayBuffer.isView(data)) {
    return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  }
  if (typeof Blob !== "undefined" && data instanceof Blob) return undefined;
  return undefined;
}

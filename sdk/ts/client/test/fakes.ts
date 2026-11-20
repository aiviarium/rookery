// The fakes the client's tests use: a socket, an endpoint pair, and a fetch that
// answers a Connect call from a message.
import type { SocketFactory, SocketLike } from "../src/socket.ts";

/** FakeSocket is a socket that a test opens, feeds and closes. */
export class FakeSocket implements SocketLike {
  binaryType = "arraybuffer";
  readyState = 0;
  readonly sent: (string | Uint8Array | ArrayBuffer)[] = [];
  closed = false;

  readonly #listeners = new Map<string, Set<(event: never) => void>>();

  constructor(readonly url: string) {}

  /** open makes the socket ready, as a WebSocket does once it is connected. */
  open(): this {
    this.readyState = 1;
    this.#dispatch("open", {});
    return this;
  }

  /** message delivers one frame from the server. */
  message(data: string | Uint8Array | ArrayBuffer): void {
    this.#dispatch("message", { data });
  }

  /** fail delivers an error. */
  fail(): void {
    this.#dispatch("error", {});
  }

  /** hangUp delivers the close a server's end causes. */
  hangUp(): void {
    this.readyState = 3;
    this.#dispatch("close", {});
  }

  send(data: ArrayBufferLike | ArrayBufferView | string): void {
    if (this.readyState !== 1) {
      throw new Error("the socket is not open");
    }
    if (typeof data === "string") {
      this.sent.push(data);
      return;
    }
    if (ArrayBuffer.isView(data)) {
      this.sent.push(
        new Uint8Array(data.buffer as ArrayBuffer, data.byteOffset, data.byteLength).slice(),
      );
      return;
    }
    this.sent.push(new Uint8Array(data).slice());
  }

  close(): void {
    this.closed = true;
    this.readyState = 3;
  }

  addEventListener(type: string, listener: (event: never) => void): void {
    let set = this.#listeners.get(type);
    if (set === undefined) {
      set = new Set();
      this.#listeners.set(type, set);
    }
    set.add(listener);
  }

  removeEventListener(type: string, listener: (event: never) => void): void {
    this.#listeners.get(type)?.delete(listener);
  }

  #dispatch(type: string, event: unknown): void {
    for (const listener of [...(this.#listeners.get(type) ?? [])]) {
      (listener as (event: unknown) => void)(event);
    }
  }
}

/** socketFactory is a factory and the sockets it opened, in order. */
export interface FakeSockets {
  readonly factory: SocketFactory;
  readonly opened: FakeSocket[];
}

/** fakeSockets makes a factory that records the sockets it opened. */
export function fakeSockets(): FakeSockets {
  const opened: FakeSocket[] = [];
  return {
    opened,
    factory: (url) => {
      const socket = new FakeSocket(url);
      opened.push(socket);
      return socket;
    },
  };
}

/** bytesOf reads one of a fake socket's sent frames as bytes. */
export function bytesOf(frame: unknown): Uint8Array {
  if (typeof frame === "string") return new TextEncoder().encode(frame);
  if (frame instanceof ArrayBuffer) return new Uint8Array(frame);
  if (ArrayBuffer.isView(frame)) {
    return new Uint8Array(frame.buffer, frame.byteOffset, frame.byteLength);
  }
  throw new Error(`not a frame: ${String(frame)}`);
}

// Live documents: a participant's CRDT updates, relayed by the server, which
// stores them without reading them. Yjs and any other CRDT work.
//
// The route is `/ws/documents/{id}`. Each binary frame is one kind byte and a
// payload:
//
// - 1, an update: a CRDT update's bytes. The server sends the latest snapshot
//   first, as an update, then every later update.
// - 2, awareness: relayed to the other participants and never stored.
//
// A text frame is a JSON control message; the server sends none yet, and a client
// ignores the ones it does not know.
import type { RookeryEndpoints, SocketRouteOptions } from "./endpoints.ts";
import { type SocketStatus, Wire } from "./socket.ts";
import type { Watchable } from "./watchable.ts";

/** DocumentFrame is what a binary frame of the document route is. */
export const DocumentFrame = {
  /** update is a CRDT update, and is stored. */
  update: 1,
  /** awareness is a participant's state, and is not stored. */
  awareness: 2,
} as const;

/** DocumentOptions configures one document's route. */
export interface DocumentOptions extends SocketRouteOptions {
  /** onError hears a socket failure. */
  onError?: (error: Error) => void;
}

/** DocumentSync is one document, synced while it is open. */
export class DocumentSync {
  /** id is the document's id. */
  readonly id: string;
  readonly #wire: Wire;
  readonly #updates = new Set<(update: Uint8Array) => void>();
  readonly #awareness = new Set<(state: Uint8Array) => void>();

  constructor(endpoints: RookeryEndpoints, id: string, options: DocumentOptions = {}) {
    this.id = id;
    this.#wire = new Wire({
      url: endpoints.webSocketUrl(`/ws/documents/${encodeURIComponent(id)}`),
      factory: options.factory,
      onError: options.onError,
      onFrame: (frame) => {
        switch (frame.kind) {
          case DocumentFrame.update:
            for (const listener of [...this.#updates]) listener(frame.payload);
            return;
          case DocumentFrame.awareness:
            for (const listener of [...this.#awareness]) listener(frame.payload);
            return;
          default:
            // A kind this client does not know comes from a newer server.
            return;
        }
      },
    });
  }

  /** status is where the socket is. */
  get status(): Watchable<SocketStatus> {
    return this.#wire.status;
  }

  /** open connects, and sends what the server asks with afterwards. */
  open(): this {
    this.#wire.open();
    return this;
  }

  /** close stops syncing. */
  close(): void {
    this.#wire.close();
    this.#updates.clear();
    this.#awareness.clear();
  }

  /** sendUpdate sends a CRDT update for the server to store and relay. */
  sendUpdate(update: Uint8Array): void {
    this.#wire.send(frame(DocumentFrame.update, update));
  }

  /** sendAwareness sends a participant's state, which the server relays only. */
  sendAwareness(state: Uint8Array): void {
    this.#wire.send(frame(DocumentFrame.awareness, state));
  }

  /** onUpdate calls listener for each update from the server. */
  onUpdate(listener: (update: Uint8Array) => void): () => void {
    this.#updates.add(listener);
    return () => {
      this.#updates.delete(listener);
    };
  }

  /** onAwareness calls listener for each other participant's awareness state. */
  onAwareness(listener: (state: Uint8Array) => void): () => void {
    this.#awareness.add(listener);
    return () => {
      this.#awareness.delete(listener);
    };
  }
}

/** frame is a kind byte and a payload, as the document route's protocol writes it. */
function frame(kind: number, payload: Uint8Array): Uint8Array {
  const bytes = new Uint8Array(payload.length + 1);
  bytes[0] = kind;
  bytes.set(payload, 1);
  return bytes;
}

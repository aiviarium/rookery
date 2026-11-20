import { describe, expect, test } from "bun:test";

import { createRookery } from "../src/transport.ts";
import { bytesOf, fakeSockets } from "./fakes.ts";

/** client is a client that reaches a made-up server and makes no calls. */
function client(space = "") {
  return createRookery({
    baseUrl: "https://rookery.test",
    fetch: (async () => {
      throw new Error("the test made no HTTP call");
    }) as unknown as typeof fetch,
    autoSignIn: false,
    space,
  });
}

describe("documents", () => {
  test("opens the document's route with the current space", () => {
    const sockets = fakeSockets();
    const document = client("01hspace").documents.open("01hdoc", { factory: sockets.factory });
    document.open();
    expect(sockets.opened[0]?.url).toBe("wss://rookery.test/ws/documents/01hdoc?space=01hspace");
  });

  test("frames updates and awareness apart, and reads both back", () => {
    const sockets = fakeSockets();
    const document = client().documents.open("d", { factory: sockets.factory }).open();
    const socket = sockets.opened[0];
    if (socket === undefined) throw new Error("the document opened no socket");
    socket.open();

    document.sendUpdate(new Uint8Array([1, 2]));
    document.sendAwareness(new Uint8Array([3]));
    expect(bytesOf(socket.sent[0])).toEqual(new Uint8Array([1, 1, 2]));
    expect(bytesOf(socket.sent[1])).toEqual(new Uint8Array([2, 3]));

    const updates: Uint8Array[] = [];
    const awareness: Uint8Array[] = [];
    document.onUpdate((update) => updates.push(update));
    document.onAwareness((state) => awareness.push(state));
    socket.message(new Uint8Array([1, 7, 8]));
    socket.message(new Uint8Array([2, 9]));
    socket.message(new Uint8Array([42, 0])); // a kind a newer server added

    expect(updates).toEqual([new Uint8Array([7, 8])]);
    expect(awareness).toEqual([new Uint8Array([9])]);
    expect(document.status.get()).toBe("open");

    document.close();
    expect(socket.closed).toBe(true);
  });
});

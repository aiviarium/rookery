import { describe, expect, test } from "bun:test";

import { createRookery } from "../src/transport.ts";
import { fakeSockets } from "./fakes.ts";

/** client is a client that reaches a made-up server and makes no calls. */
function client() {
  return createRookery({
    baseUrl: "https://rookery.test",
    fetch: (async () => {
      throw new Error("the test made no HTTP call");
    }) as unknown as typeof fetch,
    autoSignIn: false,
    space: "01hspace",
  });
}

describe("desktop", () => {
  test("names a host's RFB route, for noVNC to open itself", () => {
    expect(client().desktop.url("forge")).toBe(
      "wss://rookery.test/ws/hosts/forge/desktop?space=01hspace",
    );
  });

  test("is a socket an RFB client can consume", () => {
    const sockets = fakeSockets();
    const desktop = client().desktop.open("forge", { factory: sockets.factory });
    const socket = sockets.opened[0];
    if (socket === undefined) throw new Error("the desktop opened no socket");
    expect(desktop.readyState).toBe(0);

    socket.open();
    expect(desktop.readyState).toBe(1);

    const messages: ArrayBuffer[] = [];
    const opened: unknown[] = [];
    desktop.onmessage = (event) => messages.push(event.data);
    desktop.addEventListener("open", (() => opened.push("open")) as never);
    socket.message(new Uint8Array([0x52, 0x46, 0x42, 0x20]));

    // The server's greeting arrives as the bytes RFB expects, in an ArrayBuffer.
    expect(new Uint8Array(messages[0] as ArrayBuffer)).toEqual(
      new Uint8Array([0x52, 0x46, 0x42, 0x20]),
    );

    desktop.send(new Uint8Array([0x01]));
    expect(socket.sent.length).toBe(1);

    let closed = false;
    desktop.onclose = () => {
      closed = true;
    };
    socket.hangUp();
    expect(closed).toBe(true);
    expect(desktop.readyState).toBe(3);
    // The listener registered after the socket opened never heard it: the wire
    // began before the client asked.
    expect(opened).toEqual([]);
  });
});

import { describe, expect, test } from "bun:test";

import { createRoot } from "solid-js";

import { createRookery } from "../src/transport.ts";
import { useDesktop, useDocument, useTerminal } from "../src/solid/routes.ts";
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

describe("useDocument", () => {
  test("reads the latest update and awareness, and closes with its owner", () => {
    const sockets = fakeSockets();
    const handle = createRoot((dispose) => {
      const document = useDocument(client("01hspace"), "01hdoc", { factory: sockets.factory });
      return { document, dispose };
    });
    const socket = sockets.opened[0];
    if (socket === undefined) throw new Error("the document opened no socket");
    expect(socket.url).toBe("wss://rookery.test/ws/documents/01hdoc?space=01hspace");
    socket.open();

    socket.message(new Uint8Array([1, 5]));
    expect(handle.document.update()).toEqual(new Uint8Array([5]));
    socket.message(new Uint8Array([2, 6]));
    expect(handle.document.awareness()).toEqual(new Uint8Array([6]));
    expect(handle.document.status()).toBe("open");

    handle.dispose();
    expect(socket.closed).toBe(true);
  });
});

describe("useTerminal", () => {
  test("reads the output and the exit code", () => {
    const sockets = fakeSockets();
    const handle = createRoot((dispose) => {
      const terminal = useTerminal(client(), "01hterminal", { factory: sockets.factory });
      return { terminal, dispose };
    });
    const socket = sockets.opened[0];
    if (socket === undefined) throw new Error("the terminal opened no socket");
    socket.open();

    socket.message(new TextEncoder().encode("hello ").buffer);
    socket.message(new TextEncoder().encode("world").buffer);
    expect(handle.terminal.output()).toBe("hello world");

    socket.message(JSON.stringify({ type: "exit", code: 0 }));
    expect(handle.terminal.exit()).toBe(0);

    handle.terminal.write("x");
    expect(bytesOf(socket.sent.at(-1))).toEqual(new TextEncoder().encode("x"));
    handle.dispose();
    expect(socket.closed).toBe(true);
  });
});

describe("useDesktop", () => {
  test("reads the socket's readyState", () => {
    const sockets = fakeSockets();
    const handle = createRoot((dispose) => {
      const desktop = useDesktop(client(), "forge", { factory: sockets.factory });
      return { desktop, dispose };
    });
    const socket = sockets.opened[0];
    if (socket === undefined) throw new Error("the desktop opened no socket");
    expect(handle.desktop.readyState()).toBe(0);
    socket.open();
    expect(handle.desktop.readyState()).toBe(1);
    socket.hangUp();
    expect(handle.desktop.readyState()).toBe(3);
    handle.dispose();
  });
});

import { describe, expect, test } from "bun:test";

import { createRookery } from "../src/transport.ts";
import { bytesOf, fakeSockets } from "./fakes.ts";

/** client is a client that reaches a made-up server and makes no calls. */
function client() {
  return createRookery({
    baseUrl: "https://rookery.test",
    fetch: (async () => {
      throw new Error("the test made no HTTP call");
    }) as unknown as typeof fetch,
    autoSignIn: false,
  });
}

describe("terminals", () => {
  test("attaches, writes bytes and text, and resizes", () => {
    const sockets = fakeSockets();
    const terminal = client().terminals.attach("01hterminal", { factory: sockets.factory }).open();
    const socket = sockets.opened[0];
    if (socket === undefined) throw new Error("the terminal opened no socket");
    expect(socket.url).toBe("wss://rookery.test/ws/terminals/01hterminal");
    socket.open();

    terminal.write("é");
    terminal.write(new Uint8Array([0x03]));
    terminal.resize(120, 40);

    expect(bytesOf(socket.sent[0])).toEqual(new Uint8Array([0xc3, 0xa9]));
    expect(bytesOf(socket.sent[1])).toEqual(new Uint8Array([0x03]));
    expect(socket.sent[2]).toBe(JSON.stringify({ type: "resize", cols: 120, rows: 40 }));
    terminal.close();
  });

  test("reads output and the exit code", () => {
    const sockets = fakeSockets();
    const terminal = client().terminals.attach("t", { factory: sockets.factory }).open();
    const socket = sockets.opened[0];
    if (socket === undefined) throw new Error("the terminal opened no socket");
    socket.open();

    const output: string[] = [];
    const exits: number[] = [];
    terminal.onData((data) => output.push(new TextDecoder().decode(data)));
    terminal.onExit((code) => exits.push(code));
    socket.message(new TextEncoder().encode("hello").buffer);
    socket.message(JSON.stringify({ type: "exit", code: 3 }));
    socket.message("not json");

    expect(output).toEqual(["hello"]);
    expect(exits).toEqual([3]);
  });
});

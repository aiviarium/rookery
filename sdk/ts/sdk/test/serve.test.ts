import { afterAll, describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { request } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { servePlugin } from "../src/serve.ts";

// The socket a test serves on is short: a Unix socket's path is limited, and
// TMPDIR under the repository is not.
const dir = mkdtempSync(join(tmpdir(), "rookery-serve-"));

afterAll(() => rmSync(dir, { recursive: true, force: true }));

describe("servePlugin", () => {
  test("serves on the socket it is given and removes it on close", async () => {
    const socket = join(dir, "plugin.sock");
    const plugin = await servePlugin(() => {}, { socket, http2: false });
    expect(plugin.socket).toBe(socket);
    expect(existsSync(socket)).toBe(true);

    // A request to a path no service answers still proves the socket answers:
    // Connect's own "no such procedure" is a response, not a refused connection.
    const status = await get(socket, "/rookery.plugin.v1.PluginService/Health");
    expect(status).toBeGreaterThanOrEqual(400);

    await plugin.close();
    expect(existsSync(socket)).toBe(false);
  });

  test("closes once, however often it is asked", async () => {
    const socket = join(dir, "twice.sock");
    const plugin = await servePlugin(() => {}, { socket, http2: false });
    await Promise.all([plugin.close(), plugin.close()]);
    expect(existsSync(socket)).toBe(false);
  });

  test("refuses a socket another process is serving on", async () => {
    const socket = join(dir, "taken.sock");
    const plugin = await servePlugin(() => {}, { socket, http2: false });
    await expect(servePlugin(() => {}, { socket, http2: false })).rejects.toThrow();
    await plugin.close();
  });
});

/** get sends a GET over the Unix socket and returns the status. */
function get(socket: string, path: string): Promise<number> {
  return new Promise((resolve, reject) => {
    const call = request({ socketPath: socket, path, method: "GET" }, (response) => {
      response.resume();
      response.on("end", () => resolve(response.statusCode ?? 0));
    });
    call.on("error", reject);
    call.end();
  });
}

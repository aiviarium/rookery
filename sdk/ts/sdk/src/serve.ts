// The socket a plugin serves on: what the server calls, and the plugin answers.
//
// The server names the socket in ROOKERY_PLUGIN_SOCKET and calls it with gRPC
// over HTTP/2 without TLS, which is how Rookery's own processes talk to each
// other. A plugin passes `servePlugin` the router its declared procedures are
// registered on; a provisioner registers its own service there.
//
// On Node the socket speaks HTTP/2, so a gRPC caller reaches it. Bun has no
// HTTP/2 server, so a plugin running on Bun speaks Connect and gRPC-Web over
// HTTP/1.1 instead, which a caller that knows the plugin's runtime picks.

import { connectNodeAdapter } from "@connectrpc/connect-node";
import type { ConnectRouter } from "@connectrpc/connect";

import { pluginConfig } from "./config.ts";

/** ServeOptions is where and how a plugin serves. */
export interface ServeOptions {
  /**
   * socket is the Unix socket to serve on, `ROOKERY_PLUGIN_SOCKET` when it is
   * not named.
   */
  readonly socket?: string;
  /**
   * http2 serves HTTP/2, which gRPC needs. It is the default on Node and
   * refused on Bun, which has no HTTP/2 server.
   */
  readonly http2?: boolean;
}

/** ServedPlugin is a plugin's socket while it serves. */
export interface ServedPlugin {
  /** socket is the path of the Unix socket it serves on. */
  readonly socket: string;
  /** close stops serving and removes the socket, and is safe to call twice. */
  close(): Promise<void>;
}

/**
 * servePlugin serves `register` on a Unix socket until `close`.
 *
 * The router answers every protocol Connect supports: Connect, gRPC-Web, and
 * gRPC when the socket speaks HTTP/2.
 */
export async function servePlugin(
  register: (router: ConnectRouter) => void,
  options: ServeOptions = {},
): Promise<ServedPlugin> {
  const socket = options.socket ?? pluginConfig({ ...process.env }).pluginSocket;
  const handler = connectNodeAdapter({ routes: register });
  const wantHTTP2 = options.http2 ?? !runningOnBun();
  const server = await listen(socket, handler, wantHTTP2);
  let closed: Promise<void> | undefined;

  return {
    socket,
    close() {
      closed ??= close(server, socket);
      return closed;
    },
  };
}

/** A socket server, as both `node:http` and `node:http2` make one. */
interface SocketServer {
  listen(path: string): void;
  close(): void;
  once(event: "error", listener: (error: Error) => void): void;
  once(event: "listening", listener: () => void): void;
}

/** listen makes the server and waits for it to listen. */
async function listen(
  socket: string,
  handler: ReturnType<typeof connectNodeAdapter>,
  http2: boolean,
): Promise<SocketServer> {
  const server = http2 ? await http2Server(handler) : await httpServer(handler);
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.once("listening", resolve);
    server.listen(socket);
  });
  return server;
}

/** close stops the server, and removes the socket it leaves behind. */
async function close(server: SocketServer, socket: string): Promise<void> {
  await new Promise<void>((resolve) => {
    server.close();
    resolve();
  });
  const { rm } = await import("node:fs/promises");
  await rm(socket, { force: true });
}

/** http2Server is an HTTP/2 server without TLS, which is what gRPC needs. */
async function http2Server(handler: ReturnType<typeof connectNodeAdapter>): Promise<SocketServer> {
  const { createServer } = await import("node:http2");
  return createServer({}, handler) as unknown as SocketServer;
}

/** httpServer is an HTTP/1.1 server, which is what Bun can serve. */
async function httpServer(handler: ReturnType<typeof connectNodeAdapter>): Promise<SocketServer> {
  const { createServer } = await import("node:http");
  return createServer(handler) as unknown as SocketServer;
}

/** runningOnBun says whether the process is Bun, which has no HTTP/2 server. */
function runningOnBun(): boolean {
  const versions = process.versions as Record<string, string | undefined>;
  return typeof versions["bun"] === "string";
}

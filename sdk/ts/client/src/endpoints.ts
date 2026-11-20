// What the routes that are not Connect need from the client: its address, the
// space it is in, and the URLs it builds. `createRookery` implements it, and the
// modules that build on it take it, which keeps them clear of the clients.
import type { SocketFactory } from "./socket.ts";

/** RookeryEndpoints is the edge's address and the caller's space. */
export interface RookeryEndpoints {
  /** baseUrl is the edge's address, without a trailing slash. */
  readonly baseUrl: string;
  /** space is the current space's id, or "" for none. */
  space(): string;
  /** url joins a path to the edge's address. */
  url(path: string): string;
  /** webSocketUrl joins a path as ws:// or wss://, with the space in the query. */
  webSocketUrl(path: string, query?: Record<string, string | undefined>): string;
}

/** SocketRouteOptions configures a route that opens a WebSocket. */
export interface SocketRouteOptions {
  /** factory opens the socket; it defaults to the browser's WebSocket. */
  factory?: SocketFactory;
}

// Rookery's browser contract.
//
// A page loads this, calls `createRookery()`, and from then on holds the edge's
// typed API: one client per service, sign-in through the cookie, the current
// space in a header, live queries, live documents, terminals, a host's desktop,
// and the loader that imports each installed plugin's client half.

export { cbor } from "./cbor.ts";
export type { RookeryEndpoints, SocketRouteOptions } from "./endpoints.ts";
export type { ClientContext, ClientHalf, HostTabMount, Mount } from "./contract.ts";
export {
  type AdminPage,
  type Contributions,
  type HostTab,
  type Nav,
  type PluginContributions,
  type View,
  type Widget,
  fetchContributions,
} from "./contributions.ts";
export { CSRF_COOKIE, CSRF_HEADER, SESSION_COOKIE, SPACE_HEADER, readCookie } from "./cookies.ts";
export { DocumentFrame, DocumentSync, type DocumentOptions } from "./documents.ts";
export {
  DesktopStream,
  type DesktopOptions,
  desktopUrl,
  openDesktop,
} from "./desktop.ts";
export {
  type LoadedClientHalf,
  type LoaderOptions,
  clientContext,
  clientHalfUrl,
  loadClientHalves,
} from "./loader.ts";
export {
  LiveEventKind,
  LiveQuery,
  type LiveChange,
  type LiveQueryOptions,
  type LiveQueryState,
  type LiveRecord,
  type LiveRequestMessage,
  type LiveResponseMessage,
  type LiveSource,
} from "./live.ts";
export {
  type Clients,
  type Rookery,
  type RookeryOptions,
  type ServiceName,
  createRookery,
  services,
} from "./transport.ts";
export {
  type AuthConfig,
  asError,
  fetchAuthConfig,
  isPermissionDenied,
  isUnauthenticated,
  type SignIn,
  type SignInSnapshot,
} from "./session.ts";
export {
  type SocketFactory,
  type SocketLike,
  type SocketStatus,
  type WireFrame,
  Wire,
  browserSocket,
  toBytes,
} from "./socket.ts";
export { TerminalSession, type TerminalControl, type TerminalOptions } from "./terminals.ts";
export { type Watchable, watchable } from "./watchable.ts";

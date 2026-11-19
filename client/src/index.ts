// @rookery/client: browser transport over /ws (JSON-RPC 2.0 + binary streams with credit),
// device-session auth helpers, plugin client-half loader, routing, and SolidJS bindings.
export type {
  AuthFetchOptions,
  SessionCheckResult,
  SessionStore,
  SignInOptions,
  SignOutOptions,
} from "./auth"
export {
  createSession,
  devLoginUrl,
  fetchSession,
  linkConfirmUrl,
  loginUrl,
  parseSignInLink,
  signOut,
} from "./auth"
export type {
  CreateStreamOptions,
  DataClient,
  DataClientCollection,
  EventSubscriptionBinding,
  Live,
  LiveQuery,
  RpcResource,
  RpcResourceOptions,
  StreamBinding,
  StreamBindingState,
} from "./bindings"
export {
  createDataClient,
  createEventSubscription,
  createLiveQuery,
  createRpcResource,
  createStream,
} from "./bindings"
export type { EventBus, EventHandler, EventSubscriptionOptions } from "./events"
export { createEventBus } from "./events"
export type * from "./generated"
export type { ClientState, PluginLoader, PluginLoaderOptions, ViewEntry } from "./loader"
export { createPluginLoader, publishSharedModules, SHARED_MODULE_GLOBAL } from "./loader"
export type { RouteEntry, RouteMatch } from "./routes"
export { buildRouteTable, matchUrl, viewUrl } from "./routes"
export type {
  ClientStream,
  ConnectionOptions,
  ConnectionPhase,
  ConnectionPhaseTimings,
  ConnState,
  StreamOptions,
} from "./ws"
export {
  BACK_MS,
  Connection,
  createConnectionPhase,
  GRACE_MS,
  INITIAL_STREAM_WINDOW,
  MAX_STREAM_CHUNK,
  OFFLINE_MS,
  wsUrlFromLocation,
} from "./ws"

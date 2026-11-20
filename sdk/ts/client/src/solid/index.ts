// `@rookery/client/solid`: the client's state and its routes as Solid 2 reads
// them — signals for what pushes, async computations for what answers, and the
// owner's lifecycle for everything that stays open.
//
// Solid 2 only: there is no 1.x compatibility layer here.
export {
  type Query,
  type SignInAccessor,
  createQuery,
  fromWatchable,
  onDispose,
  useSignIn,
} from "./signals.ts";
export { type LiveQueryHandle, useLiveQuery } from "./live.ts";
export {
  type DesktopHandle,
  type DocumentHandle,
  type TerminalHandle,
  useDesktop,
  useDocument,
  useTerminal,
} from "./routes.ts";

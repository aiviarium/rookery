// @rookery/sdk/client: the client half's API. The shell provides this module to plugin bundles via
// globalThis.__rookery.modules, so every plugin shares one copy.
//
//   import { defineClient } from "@rookery/sdk/client"
//   export default defineClient((rookery) => {
//     rookery.views.register("hello.main", () => <p>Hello</p>)
//   })
import type { ClientRookery } from "./api"
import type { ClientPlugin } from "./runtime"

export { z } from "zod"
export type {
  BatchOp,
  Collection,
  LogCollection,
  Page,
  Query,
  RichText,
  RichTextNode,
  Row,
  Where,
} from "../data"
export { mentions, plainText } from "../data"
export { type ErrorCode, RookeryError } from "../errors"
export type * from "../generated"
export type { Disposable } from "../types"
export type {
  AppServices,
  Blobs,
  Chrome,
  ClientEnv,
  ClientRookery,
  ClientTerminals,
  Commands,
  Connections,
  Contexts,
  Data,
  Events,
  Extensions,
  Host,
  JobFollower,
  Jobs,
  LiveCollection,
  Nav,
  Plugins,
  Rpc,
  Settings,
  Showable,
  Spaces,
  Storage,
  Streams,
  Views,
  Workbench,
} from "./api"
export { serviceUrl } from "./api"
export type { Live, LiveOptions, LiveQuery } from "./live"
export { clearLiveCache, createLive } from "./live"
export type {
  AdminPageEntry,
  ChromeComponentProps,
  ClientCommandDef,
  ClientPlugin,
  ClientRuntime,
  ClientStream,
  ContextProvider,
  ContextValueInfo,
  DesktopViewProps,
  EmbeddedViewProps,
  FieldEditorProps,
  FieldPartProps,
  FieldParts,
  NavItem,
  NavSection,
  NavSectionProps,
  OpenViewOptions,
  RecordSectionProps,
  ShellComponents,
  StreamOptions,
  Toast,
  ViewComponent,
  ViewHandle,
  ViewOptions,
  ViewProps,
} from "./runtime"

/** Declares a plugin's client half. The function runs in the shell when the plugin loads. */
export function defineClient(activate: (rookery: ClientRookery) => void | Promise<void>): ClientPlugin {
  return Object.freeze({ __rookery: "client" as const, activate })
}

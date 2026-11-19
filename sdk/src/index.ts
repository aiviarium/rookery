// @rookery/sdk: the host half's plugin API and shared types.
//
//   import { definePlugin, z } from "@rookery/sdk"
//   export default definePlugin((rookery) => {
//     rookery.commands.register("hello.greet", {
//       params: z.object({ name: z.string() }),
//       run: (ctx, { name }) => `Hello, ${name}`,
//     })
//   })
import type { HostPlugin, Rookery } from "./host/types"

export { z } from "zod"
export type {
  AggregateQuery,
  BatchOp,
  BatchOptions,
  Collection,
  CollectionsOf,
  Condition,
  FieldOps,
  ImportOptions,
  ImportResult,
  Insert,
  LogCollection,
  Measure,
  MigrationTx,
  OrderKey,
  Page,
  Patch,
  Query,
  Refs,
  RichText,
  RichTextNode,
  Row,
  ScanQuery,
  SystemFields,
  TailOptions,
  Where,
} from "./data"
export { binaryRow, collectionsProxy, createCollection, createRefs, mentions, plainText } from "./data"
export { codeForNumber, ERROR_NUMBERS, type ErrorCode, isErrorCode, RookeryError } from "./errors"
export type { Filter, FilterCollection, FilterField, OrderBy } from "./filter"
export {
  columnKind,
  compareCodePoints,
  compareRows,
  normalizeRef,
  parseDate,
  parseDatetime,
  parseFilter,
} from "./filter"
export type * from "./generated"
export type {
  Access,
  AppServices,
  Audit,
  BlobOptions,
  Blobs,
  ChangeEvent,
  Changes,
  Commands,
  Connections,
  Ctx,
  EnqueueOptions,
  Events,
  Extensions,
  HostCommandDef,
  HostEnv,
  HostPlugin,
  JobDef,
  JobHandle,
  Jobs,
  Lease,
  LeaseOptions,
  Leases,
  Logger,
  Notice,
  NoticeKind,
  Notify,
  Rookery,
  RookeryStream,
  Rpc,
  RpcHandlerDef,
  RpcLevel,
  Schedules,
  SecretOptions,
  Secrets,
  ServiceData,
  Services,
  Settings,
  Spaces,
  Storage,
  StorageOptions,
  Streams,
  SubscribeOptions,
  Terminals,
  Topics,
} from "./host/types"
export { isHostPlugin } from "./host/types"
export type {
  Collections,
  CommandId,
  Disposable,
  OptionalServices,
  SettingKey,
  TopicPayloads,
  ViewId,
} from "./types"
export { disposable } from "./types"
export type {
  NOT_ON_SERVER,
  NotificationMethod,
  NotificationParams,
  ServerEventName,
  ServerEvents,
  WireCall,
  WireMethod,
  WireNotifications,
  WireParams,
  WireResult,
  WireTransport,
} from "./wire"
export { WIRE, wireCall } from "./wire"

/**
 * Types generated from the plugin's manifest. rookery-env.d.ts in a plugin folder augments this interface
 * with the plugin's settings, commands, views, collections, topics and dependency services.
 */
// biome-ignore lint/suspicious/noEmptyInterface: augmented by rookery-env.d.ts
export interface RookeryEnv {}

/** Declares a plugin's host half. The function runs when the plugin activates; it may return a cleanup function. */
export function definePlugin(activate: (rookery: Rookery) => unknown): HostPlugin {
  return Object.freeze({ __rookery: "host" as const, activate })
}

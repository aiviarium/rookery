// The methods `@rookery/client` and `@rookery/sdk` call, with the params and result of each from the
// generated wire types of `rookery serve` (`client/src/generated/`, written from `rookery-proto`).
//
// A call in this map is typed end to end: a changed field, a new required field or a different result
// in `rookery-proto` fails `bun run check` at the call site. A method outside it is one the server
// does not have yet, which `sdk/test/wire.test.ts` keeps honest against `api/methods.json`.
import type {
  BlobInfo,
  BlobRef,
  ConnectionCreate,
  ConnectionInfo,
  ConnectionList,
  ConnectionsOf,
  ConnectionTested,
  ConnectionTypes,
  ConnectionUpdate,
  Count,
  Deleted,
  Delivered,
  Empty,
  EventNotice,
  EventPublish,
  EventSubscribe,
  EventSubscribed,
  EventsDropped,
  EventUnsubscribe,
  IdRef,
  JobChanged,
  JobEnqueue,
  JobFollow,
  JobInfo,
  JobList,
  JobQuery,
  JobRef,
  LogItems,
  Me,
  NotificationInfo,
  NotificationList,
  NotifyAnswer,
  NotifyChanged,
  NotifyNew,
  NotifyPref,
  NotifyQuery,
  NotifyRead,
  NotifySend,
  NotifySent,
  Revision,
  SettingList,
  SettingRef,
  SettingReset,
  SettingSet,
  SettingsOf,
  SettingValue,
  StorageEntries,
  StorageList,
  StorageRef,
  StorageSet,
  StorageValue,
  StreamClose,
  StreamCredit,
  StreamEof,
  StreamOpened,
} from "./generated"

/** The params and the result of one method. */
export interface WireCall<P, R> {
  readonly params: P
  readonly result: R
}

/**
 * The methods the client calls that `rookery serve` has, with the generated types of each. The values
 * are empty at runtime: this is a table of types that a test and `wireCall` read the names from.
 */
export const WIRE = {
  "auth.me": {} as WireCall<Empty, Me>,
  "blobs.get": {} as WireCall<BlobRef, BlobInfo>,
  "connections.create": {} as WireCall<ConnectionCreate, ConnectionInfo>,
  "connections.delete": {} as WireCall<IdRef, Empty>,
  "connections.get": {} as WireCall<IdRef, ConnectionInfo>,
  "connections.list": {} as WireCall<ConnectionsOf, ConnectionList>,
  "connections.test": {} as WireCall<IdRef, ConnectionTested>,
  "connections.types": {} as WireCall<Empty, ConnectionTypes>,
  "connections.update": {} as WireCall<ConnectionUpdate, ConnectionInfo>,
  "events.publish": {} as WireCall<EventPublish, Delivered>,
  "events.subscribe": {} as WireCall<EventSubscribe, EventSubscribed>,
  "events.unsubscribe": {} as WireCall<EventUnsubscribe, Empty>,
  "jobs.cancel": {} as WireCall<JobRef, JobInfo>,
  "jobs.enqueue": {} as WireCall<JobEnqueue, JobInfo>,
  "jobs.follow": {} as WireCall<JobFollow, StreamOpened>,
  "jobs.get": {} as WireCall<JobRef, JobInfo>,
  "jobs.list": {} as WireCall<JobQuery, JobList>,
  "jobs.output": {} as WireCall<JobFollow, LogItems>,
  "jobs.retry": {} as WireCall<JobRef, JobInfo>,
  "notify.answer": {} as WireCall<NotifyAnswer, NotificationInfo>,
  "notify.list": {} as WireCall<NotifyQuery, NotificationList>,
  "notify.read": {} as WireCall<NotifyRead, Count>,
  "notify.send": {} as WireCall<NotifySend, NotifySent>,
  "notify.setPref": {} as WireCall<NotifyPref, Empty>,
  "settings.get": {} as WireCall<SettingRef, SettingValue>,
  "settings.list": {} as WireCall<SettingsOf, SettingList>,
  "settings.reset": {} as WireCall<SettingReset, Empty>,
  "settings.set": {} as WireCall<SettingSet, Empty>,
  "storage.delete": {} as WireCall<StorageRef, Deleted>,
  "storage.get": {} as WireCall<StorageRef, StorageValue>,
  "storage.list": {} as WireCall<StorageList, StorageEntries>,
  "storage.set": {} as WireCall<StorageSet, Revision>,
  "streams.close": {} as WireCall<StreamClose, Empty>,
  "streams.credit": {} as WireCall<StreamCredit, Empty>,
  "streams.eof": {} as WireCall<StreamEof, Empty>,
}

/** The name of a method `rookery serve` has and the client calls. */
export type WireMethod = keyof typeof WIRE
export type WireParams<M extends WireMethod> = (typeof WIRE)[M]["params"]
export type WireResult<M extends WireMethod> = (typeof WIRE)[M]["result"]

/** What `wireCall` needs: a JSON-RPC call of a method by name. */
export interface WireTransport {
  call(method: string, params: Record<string, unknown>): Promise<unknown>
}

/**
 * Calls a method of the map, with its params checked against the generated types and its result
 * typed. The cast is the map's own promise: the server answers with the generated type.
 */
export function wireCall<M extends WireMethod>(
  transport: WireTransport,
  method: M,
  params: WireParams<M>,
): Promise<WireResult<M>> {
  return transport.call(method, params as Record<string, unknown>) as Promise<WireResult<M>>
}

/**
 * The notifications of `rookery serve`, with the generated types of their params.
 *
 * `jobs.changed` and `notify.changed` are the prototype's; no code in `rookery serve` sends either
 * yet, so a listener hears nothing until the server does.
 */
export interface WireNotifications {
  "events.event": EventNotice
  "events.dropped": EventsDropped
  "notify.new": NotifyNew
  "jobs.changed": JobChanged
  "notify.changed": NotifyChanged
}

/**
 * The notifications the client listens for, for a test that checks the names it listens to. The last
 * five are what the prototype sent and no code in `rookery serve` sends yet.
 */
export const NOTIFICATIONS = [
  "events.event",
  "events.dropped",
  "notify.new",
  "jobs.changed",
  "notify.changed",
  "data.changed",
  "plugins.changed",
  "plugins.reloaded",
] as const

/** The name of a notification `rookery serve` sends. */
export type NotificationMethod = keyof WireNotifications
export type NotificationParams<M extends NotificationMethod> = WireNotifications[M]

/**
 * The methods `@rookery/client` and `@rookery/sdk` call that `rookery serve` has no method for, and
 * the milestone of `plans/v1.md` that brings each. `sdk/test/wire.test.ts` fails when one of them
 * appears in `api/methods.json`, so that it moves into `WIRE` instead of staying untyped.
 */
export const NOT_ON_SERVER = [
  // The plugin system and the extension host.
  "commands.list",
  "commands.run",
  "extensions.list",
  "plugins.activate",
  "plugins.list",
  "plugins.rpc",
  "services.list",
  "services.status",
  // The data layer and its live queries, which `createCollection` and `createLive` call.
  "data.aggregate",
  "data.batch",
  "data.count",
  "data.delete",
  "data.find",
  "data.get",
  "data.history",
  "data.import",
  "data.insert",
  "data.live",
  "data.purge",
  "data.refsFrom",
  "data.refsTo",
  "data.restore",
  "data.scan",
  "data.search",
  "data.tail",
  "data.unlive",
  "data.update",
  "data.upsert",
  // Environments and terminals, which the agent milestone brings.
  "env.connect",
  "env.deleteFile",
  "env.exec",
  "env.get",
  "env.list",
  "env.listDir",
  "env.presence",
  "env.readFile",
  "env.writeFile",
  "terminals.attach",
  "terminals.close",
  "terminals.get",
  "terminals.list",
  "terminals.open",
  "terminals.resize",
  "terminals.screen",
  "terminals.signal",
  "terminals.write",
  // Spaces, and stream channels a client opens for itself.
  "spaces.create",
  "spaces.enter",
  "spaces.get",
  "spaces.list",
  "spaces.rename",
  "streams.open",
] as const

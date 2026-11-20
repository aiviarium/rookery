// The methods `@rookery/client` and `@rookery/sdk` call, with the params and result of each from the
// generated wire types of `rookery serve` (`client/src/generated/`, written from `rookery-proto`).
//
// A call in this map is typed end to end: a changed field, a new required field or a different result
// in `rookery-proto` fails `bun run check` at the call site. A method outside it is one the server
// does not have yet, which `sdk/test/wire.test.ts` keeps honest against `api/methods.json`.
import type {
  ActivityList,
  ActivityQuery,
  AppServiceList,
  AppServiceStatus,
  BacklinkList,
  BlobInfo,
  BlobRef,
  Checkout,
  CheckoutCreate,
  CheckoutList,
  CheckoutRemove,
  Connect,
  ConnectionCreate,
  ConnectionInfo,
  ConnectionList,
  ConnectionsOf,
  ConnectionTested,
  ConnectionTypes,
  ConnectionUpdate,
  Count,
  DataAggregate,
  DataAggregateResult,
  DataBatch,
  DataBatchResult,
  DataChanged,
  DataCount,
  DataFind,
  DataImport,
  DataImportResult,
  DataInsert,
  DataPage,
  DataRow,
  DataRowRef,
  DataSearch,
  DataTail,
  DataTailResult,
  DataUpdate,
  DataUpsert,
  Deleted,
  Delivered,
  DirListing,
  DocumentContent,
  DocumentConvert,
  DocumentConverted,
  DocumentCreate,
  DocumentEdit,
  DocumentEdited,
  DocumentInfo,
  DocumentList,
  DocumentListQuery,
  DocumentOpen,
  DocumentOpened,
  DocumentRead,
  DocumentRef,
  DocumentRestore,
  DocumentSaveVersion,
  DocumentVersion,
  DocumentVersions,
  Empty,
  EntityAlias,
  EntityInfo,
  EntityList,
  EntityRef,
  EntityRefs,
  EntityRegister,
  EntitySearch,
  EntityTextList,
  EntityTextQuery,
  EnvChanged,
  EnvFileWrite,
  EnvInfo,
  EnvList,
  EnvPresence,
  EnvRef,
  EnvSessionList,
  EnvSessionQuery,
  EventNotice,
  EventPublish,
  EventSubscribe,
  EventSubscribed,
  EventsDropped,
  EventUnsubscribe,
  ExecParams,
  ExecResult,
  ExtensionList,
  FileData,
  FileDelete,
  FilePath,
  FileStat,
  FileWritten,
  HistoryList,
  HostInfo,
  IdRef,
  JobChanged,
  JobEnqueue,
  JobFollow,
  JobInfo,
  JobList,
  JobQuery,
  JobRef,
  LevelHeld,
  LevelOf,
  LiveQuery,
  LiveStarted,
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
  PersonList,
  PinAdd,
  PinInfo,
  PinList,
  PinRemove,
  PinRemoved,
  PinsChanged,
  PinsQuery,
  PluginInfo,
  PluginList,
  PortRef,
  PortUrl,
  RecentForget,
  RecentsList,
  RecentsQuery,
  RecentTouch,
  RecentTouched,
  ReferenceList,
  RefsOf,
  Revision,
  ScheduleCreate,
  ScheduleFire,
  ScheduleInfo,
  ScheduleList,
  ScheduleListQuery,
  ScheduleRef,
  ScheduleSetEnabled,
  ScreenInfo,
  ScreenQuery,
  SearchParams,
  SearchResult,
  SettingList,
  SettingRef,
  SettingReset,
  SettingSet,
  SettingsOf,
  SettingValue,
  SpaceCreate,
  SpaceInfo,
  SpaceKindList,
  SpaceList,
  SpaceRef,
  SpaceRename,
  SqlBegin,
  SqlCredential,
  SqlCredentialRef,
  SqlExecuted,
  SqlPublished,
  SqlQuery,
  SqlRows,
  SqlStatement,
  SqlStreamed,
  SqlTx,
  SqlTxPublish,
  SqlTxRef,
  SqlTxStatement,
  StorageEntries,
  StorageList,
  StorageRef,
  StorageSet,
  StorageValue,
  StreamClose,
  StreamCredit,
  StreamEof,
  StreamOpened,
  SubscriptionRef,
  TerminalAdopt,
  TerminalAttach,
  TerminalAttached,
  TerminalChanged,
  TerminalInfo,
  TerminalList,
  TerminalOpen,
  TerminalQuery,
  TerminalRef,
  TerminalRename,
  TerminalResize,
  TerminalSignal,
  TerminalWrite,
  UserSearch,
  WebhookList,
  WebhookSecretSet,
  WorktreeCreate,
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
  "access.levelOf": {} as WireCall<LevelOf, LevelHeld>,
  "activity.recent": {} as WireCall<ActivityQuery, ActivityList>,
  "auth.me": {} as WireCall<Empty, Me>,
  "blobs.get": {} as WireCall<BlobRef, BlobInfo>,
  "connections.create": {} as WireCall<ConnectionCreate, ConnectionInfo>,
  "connections.delete": {} as WireCall<IdRef, Empty>,
  "connections.get": {} as WireCall<IdRef, ConnectionInfo>,
  "connections.list": {} as WireCall<ConnectionsOf, ConnectionList>,
  "connections.test": {} as WireCall<IdRef, ConnectionTested>,
  "connections.types": {} as WireCall<Empty, ConnectionTypes>,
  "connections.update": {} as WireCall<ConnectionUpdate, ConnectionInfo>,
  "data.aggregate": {} as WireCall<DataAggregate, DataAggregateResult>,
  "data.batch": {} as WireCall<DataBatch, DataBatchResult>,
  "data.count": {} as WireCall<DataFind, DataCount>,
  "data.delete": {} as WireCall<DataRowRef, DataRow>,
  "data.find": {} as WireCall<DataFind, DataPage>,
  "data.get": {} as WireCall<DataRowRef, DataRow>,
  "data.history": {} as WireCall<DataRowRef, HistoryList>,
  "data.import": {} as WireCall<DataImport, DataImportResult>,
  "data.insert": {} as WireCall<DataInsert, DataRow>,
  "data.live": {} as WireCall<LiveQuery, LiveStarted>,
  "data.purge": {} as WireCall<DataRowRef, DataRow>,
  "data.refsFrom": {} as WireCall<RefsOf, ReferenceList>,
  "data.refsTo": {} as WireCall<RefsOf, BacklinkList>,
  "data.restore": {} as WireCall<DataRowRef, DataRow>,
  "data.scan": {} as WireCall<DataFind, DataPage>,
  "data.search": {} as WireCall<DataSearch, DataPage>,
  "data.tail": {} as WireCall<DataTail, DataTailResult>,
  "data.unlive": {} as WireCall<SubscriptionRef, Empty>,
  "data.update": {} as WireCall<DataUpdate, DataRow>,
  "data.upsert": {} as WireCall<DataUpsert, DataRow>,
  "documents.convert": {} as WireCall<DocumentConvert, DocumentConverted>,
  "documents.create": {} as WireCall<DocumentCreate, DocumentInfo>,
  "documents.delete": {} as WireCall<DocumentRef, null>,
  "documents.edit": {} as WireCall<DocumentEdit, DocumentEdited>,
  "documents.flush": {} as WireCall<DocumentRef, null>,
  "documents.get": {} as WireCall<DocumentRef, DocumentInfo>,
  "documents.history": {} as WireCall<DocumentRef, DocumentVersions>,
  "documents.list": {} as WireCall<DocumentListQuery, DocumentList>,
  "documents.open": {} as WireCall<DocumentOpen, DocumentOpened>,
  "documents.read": {} as WireCall<DocumentRead, DocumentContent>,
  "documents.restore": {} as WireCall<DocumentRestore, DocumentVersion>,
  "documents.saveVersion": {} as WireCall<DocumentSaveVersion, DocumentVersion>,
  "entities.alias": {} as WireCall<EntityAlias, Empty>,
  "entities.register": {} as WireCall<EntityRegister, EntityInfo>,
  "entities.remove": {} as WireCall<EntityRef, Empty>,
  "entities.resolve": {} as WireCall<EntityRef, EntityInfo | null>,
  "entities.resolveMany": {} as WireCall<EntityRefs, EntityList>,
  "entities.search": {} as WireCall<EntitySearch, EntityList>,
  "entities.text": {} as WireCall<EntityTextQuery, EntityTextList>,
  "env.checkouts": {} as WireCall<EnvRef, CheckoutList>,
  "env.clone": {} as WireCall<CheckoutCreate, Checkout>,
  "env.connect": {} as WireCall<Connect, StreamOpened>,
  "env.deleteFile": {} as WireCall<FileDelete, Empty>,
  "env.exec": {} as WireCall<ExecParams, ExecResult>,
  "env.get": {} as WireCall<EnvRef, EnvInfo>,
  "env.list": {} as WireCall<Empty, EnvList>,
  "env.listDir": {} as WireCall<FilePath, DirListing>,
  "env.portUrl": {} as WireCall<PortRef, PortUrl>,
  "env.presence": {} as WireCall<EnvRef, EnvPresence>,
  "env.readFile": {} as WireCall<FilePath, FileData>,
  "env.removeCheckout": {} as WireCall<CheckoutRemove, Empty>,
  "env.stat": {} as WireCall<FilePath, FileStat>,
  "env.worktree": {} as WireCall<WorktreeCreate, Checkout>,
  "env.writeFile": {} as WireCall<EnvFileWrite, FileWritten>,
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
  "extensions.list": {} as WireCall<{ point: string }, ExtensionList>,
  "pins.add": {} as WireCall<PinAdd, PinInfo>,
  "pins.list": {} as WireCall<PinsQuery, PinList>,
  "pins.remove": {} as WireCall<PinRemove, PinRemoved>,
  "plugins.activate": {} as WireCall<{ id: string }, PluginInfo>,
  "plugins.list": {} as WireCall<Empty, PluginList>,
  "plugins.rpc": {} as WireCall<
    { plugin: string; method: string; params?: Record<string, unknown> },
    unknown
  >,
  "recents.forget": {} as WireCall<RecentForget, Empty>,
  "recents.list": {} as WireCall<RecentsQuery, RecentsList>,
  "recents.touch": {} as WireCall<RecentTouch, RecentTouched>,
  "schedules.create": {} as WireCall<ScheduleCreate, ScheduleInfo>,
  "schedules.fire": {} as WireCall<ScheduleFire, { job: string }>,
  "schedules.list": {} as WireCall<ScheduleListQuery, ScheduleList>,
  "schedules.remove": {} as WireCall<ScheduleRef, Empty>,
  "schedules.setEnabled": {} as WireCall<ScheduleSetEnabled, ScheduleInfo>,
  "search.query": {} as WireCall<SearchParams, SearchResult>,
  "services.call": {} as WireCall<
    { plugin: string; service: string; method: string; params?: Record<string, unknown> },
    unknown
  >,
  "services.stream": {} as WireCall<
    { plugin: string; service: string; method: string; params?: Record<string, unknown> },
    StreamOpened
  >,
  "services.list": {} as WireCall<{ plugin?: string }, AppServiceList>,
  "services.status": {} as WireCall<{ plugin: string; service: string }, AppServiceStatus>,
  "settings.get": {} as WireCall<SettingRef, SettingValue>,
  "settings.list": {} as WireCall<SettingsOf, SettingList>,
  "settings.reset": {} as WireCall<SettingReset, Empty>,
  "settings.set": {} as WireCall<SettingSet, Empty>,
  "spaces.create": {} as WireCall<SpaceCreate, SpaceInfo>,
  "spaces.enter": {} as WireCall<SpaceRef, SpaceInfo>,
  "spaces.get": {} as WireCall<SpaceRef, SpaceInfo>,
  "spaces.kinds": {} as WireCall<Empty, SpaceKindList>,
  "spaces.list": {} as WireCall<Empty, SpaceList>,
  "spaces.rename": {} as WireCall<SpaceRename, SpaceInfo>,
  "sql.begin": {} as WireCall<SqlBegin, SqlTx>,
  "sql.commit": {} as WireCall<SqlTxRef, Empty>,
  "sql.credential": {} as WireCall<SqlCredentialRef, SqlCredential>,
  "sql.execute": {} as WireCall<SqlStatement, SqlExecuted>,
  "sql.query": {} as WireCall<SqlQuery, SqlRows>,
  "sql.queryStream": {} as WireCall<SqlQuery, SqlStreamed>,
  "sql.rollback": {} as WireCall<SqlTxRef, Empty>,
  "sql.rotateCredential": {} as WireCall<SqlCredentialRef, SqlCredential>,
  "sql.txExecute": {} as WireCall<SqlTxStatement, SqlExecuted>,
  "sql.txPublish": {} as WireCall<SqlTxPublish, SqlPublished>,
  "sql.txQuery": {} as WireCall<SqlTxStatement, SqlRows>,
  "storage.delete": {} as WireCall<StorageRef, Deleted>,
  "storage.get": {} as WireCall<StorageRef, StorageValue>,
  "storage.list": {} as WireCall<StorageList, StorageEntries>,
  "storage.set": {} as WireCall<StorageSet, Revision>,
  "streams.close": {} as WireCall<StreamClose, Empty>,
  "streams.credit": {} as WireCall<StreamCredit, Empty>,
  "streams.eof": {} as WireCall<StreamEof, Empty>,
  "terminals.adopt": {} as WireCall<TerminalAdopt, TerminalInfo>,
  "terminals.attach": {} as WireCall<TerminalAttach, TerminalAttached>,
  "terminals.close": {} as WireCall<TerminalRef, Empty>,
  "terminals.get": {} as WireCall<TerminalRef, TerminalInfo>,
  "terminals.list": {} as WireCall<TerminalQuery, TerminalList>,
  "terminals.open": {} as WireCall<TerminalOpen, TerminalInfo>,
  "terminals.rename": {} as WireCall<TerminalRename, Empty>,
  "terminals.resize": {} as WireCall<TerminalResize, Empty>,
  "terminals.screen": {} as WireCall<ScreenQuery, ScreenInfo>,
  "terminals.sessions": {} as WireCall<EnvSessionQuery, EnvSessionList>,
  "terminals.signal": {} as WireCall<TerminalSignal, Empty>,
  "terminals.write": {} as WireCall<TerminalWrite, Empty>,
  "users.search": {} as WireCall<UserSearch, PersonList>,
  "webhooks.list": {} as WireCall<Empty, WebhookList>,
  "webhooks.setSecret": {} as WireCall<WebhookSecretSet, Empty>,
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
 * The notifications of `rookery serve`, with the generated types of their params. What changed among
 * hosts, resources and jobs comes as events of the server instead: see [`ServerEvents`].
 */
export interface WireNotifications {
  "data.changed": DataChanged
  "pins.changed": PinsChanged
  "events.event": EventNotice
  "events.dropped": EventsDropped
  "notify.new": NotifyNew
  "notify.changed": NotifyChanged
}

/**
 * The events `rookery serve` publishes, under its own plugin id `rookery`, with their payloads. A
 * plugin subscribes to them by these names, as to any other plugin's events (`events.subscribe`), and
 * gets those about what it may see.
 */
export interface ServerEvents {
  /** A saved host was added, changed or removed. `info` is the host as `hosts.get` gives it. */
  "rookery/hosts.changed": {
    host: string
    info: HostInfo | null
    change: string
    from?: string | null
    removed: boolean
  }
  /** A resource of a provisioner was made, changed or removed. */
  "rookery/resources.changed": { resource: string; provisioner: string; kind: string; change: string }
  /** A job was enqueued, ran, or ended. */
  "rookery/jobs.changed": JobChanged
  /** An environment (saved host) changed. */
  "rookery/env.changed": EnvChanged
  /** A terminal changed. */
  "rookery/terminals.changed": TerminalChanged
}

/** The name of an event `rookery serve` publishes. */
export type ServerEventName = keyof ServerEvents

/**
 * The notifications the client listens for, for a test that checks the names it listens to. All but
 * `plugins.reloaded` are sent by `rookery serve`; that one is declared for a shell that hears of a
 * reload without a change of the plugin's files.
 */
export const NOTIFICATIONS = [
  "data.changed",
  "pins.changed",
  "events.event",
  "events.dropped",
  "notify.new",
  "notify.changed",
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
  // Commands in the extension host.
  "commands.list",
  "commands.run",
  // Stream channels a client opens for itself.
  "streams.open",
] as const

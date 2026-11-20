// A server the shell's tests talk to, without a network: a Connect router that
// answers each service from a made-up site.
//
// Every call a page makes goes through the client's own transport, so the tests
// exercise the same path a browser does, with the answers a test sets. The site
// is mutable: a call that changes something changes it for the next read.
import { Code, ConnectError, type Transport, createRouterTransport } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import { TimestampSchema } from "@bufbuild/protobuf/wkt";
import { type LoadedClientHalf, type Rookery, createRookery } from "@rookery/client";
import {
  AccessService,
  AuditService,
  AuthService,
  ConnectionService,
  DataService,
  DocumentService,
  EventService,
  GroupService,
  HostService,
  JobService,
  KeyService,
  NotificationService,
  PluginService,
  ProvisionerService,
  RegistryService,
  SecretService,
  SettingService,
  SpaceService,
  TailnetService,
  UserService,
} from "@rookery/proto";
import type {
  Action,
  AuditEntry,
  Collection,
  Connection,
  ConnectionType,
  Document,
  DocumentSnapshot,
  Event,
  Grant,
  Group,
  Host,
  Job,
  JobLine,
  KnownHost,
  Key,
  Notification,
  Plugin,
  PluginContribution,
  ProvisionerEntry,
  ProvisionerResource,
  ProvisionerType,
  Registry,
  Secret,
  Setting,
  Space,
  TailnetDevice,
  User,
} from "@rookery/proto";

/** TEST_AT is the timestamp the fake server stamps things with. */
const TEST_AT = create(TimestampSchema, { seconds: 1_700_000_000n });

/** FakeSite is what the made-up server holds. A test changes it before mounting. */
export interface FakeSite {
  /** user is who is signed in, or undefined for nobody. */
  user?: User;
  spaces?: Space[];
  spaceKinds?: { id: string; label: string; plugin: string }[];
  plugins?: Plugin[];
  contributions?: PluginContribution[];
  hosts?: Host[];
  /** actions are one host's actions, as ListActions gives them. */
  actions?: Action[];
  /** types, provisioners and machines are what the machines page reads. */
  types?: ProvisionerType[];
  provisioners?: ProvisionerEntry[];
  machines?: ProvisionerResource[];
  /** logs are a job's lines as Follow streams them; jobEnd ends the stream. */
  logs?: JobLine[];
  jobEnd?: Job;
  jobs?: Job[];
  notifications?: Notification[];
  /** events are sent on the event stream, one at a time. */
  events?: Event[];

  users?: User[];
  groups?: Group[];
  grants?: Grant[];

  settings?: Setting[];
  secrets?: Secret[];
  connectionTypes?: ConnectionType[];
  connections?: Connection[];
  keys?: Key[];
  knownHosts?: KnownHost[];
  tailnet?: { configured: boolean; online: boolean; controlUrl: string; tag: string; addresses: string[] };
  devices?: TailnetDevice[];
  registries?: Registry[];

  collections?: Collection[];
  /** records are keyed by "plugin/collection". */
  records?: Record<string, Record<string, unknown>[]>;
  history?: Record<string, Uint8Array[]>;
  documents?: Document[];
  snapshots?: DocumentSnapshot[];
  audit?: AuditEntry[];
}

/** STATIC_USER is the person the tests sign in. */
export const STATIC_USER: User = {
  $typeName: "rookery.v1.User",
  id: "01huser",
  email: "ada@example.test",
  name: "Ada Lovelace",
  admin: true,
  disabled: false,
} as User;

/** A_SPACE and ANOTHER_SPACE are the two spaces the tests may switch between. */
export const A_SPACE: Space = {
  $typeName: "rookery.v1.Space",
  id: "01hspace",
  kind: "team",
  name: "Analysis",
  archived: false,
  createdBy: "01huser",
} as Space;

export const ANOTHER_SPACE: Space = {
  $typeName: "rookery.v1.Space",
  id: "01hspace2",
  kind: "team",
  name: "Engines",
  archived: false,
  createdBy: "01huser",
} as Space;

/** fakeTransport answers the services the shell calls from a site. */
export function fakeTransport(site: FakeSite): Transport {
  // The site's own lists are what the answers read and write, so a test that
  // holds the site sees what a call changed.
  const notifications = (site.notifications ??= []);
  const spaces = (site.spaces ??= []);
  const users = (site.users ??= []);
  const groups = (site.groups ??= []);
  const grants = (site.grants ??= []);
  const plugins = (site.plugins ??= []);
  const settings = (site.settings ??= []);
  const secrets = (site.secrets ??= []);
  const connections = (site.connections ??= []);
  const keys = (site.keys ??= []);
  const knownHosts = (site.knownHosts ??= []);
  const registries = (site.registries ??= []);
  const documents = (site.documents ??= []);
  const records = (site.records ??= {});
  let tailnet = site.tailnet ?? { configured: false, online: false, controlUrl: "", tag: "", addresses: [] };

  return createRouterTransport((router) => {
    router.service(AuthService, {
      me() {
        if (site.user === undefined) throw new ConnectError("not signed in", Code.Unauthenticated);
        return { user: site.user };
      },
      listSessions: () => ({ sessions: [] }),
      revokeSession: () => ({}),
      createToken: () => ({ token: "", secret: "" }),
      listTokens: () => ({ tokens: [] }),
      revokeToken: () => ({}),
    });

    router.service(SpaceService, {
      listKinds: () => ({ kinds: site.spaceKinds ?? [{ id: "team", label: "Team", plugin: "" }] }),
      list: () => ({ spaces, nextCursor: "" }),
      get: (request) => ({ space: spaces.find((space) => space.id === request.space) }),
      create(request) {
        const space = {
          $typeName: "rookery.v1.Space",
          id: `01hspace${spaces.length + 2}`,
          kind: request.kind,
          name: request.name,
          archived: false,
          createdBy: STATIC_USER.id,
          createdAt: TEST_AT,
          updatedAt: TEST_AT,
        } as Space;
        spaces.push(space);
        return { space };
      },
      rename(request) {
        const space = spaces.find((candidate) => candidate.id === request.space);
        if (space !== undefined) space.name = request.name;
        return { space };
      },
      archive(request) {
        const space = spaces.find((candidate) => candidate.id === request.space);
        if (space !== undefined) space.archived = request.archived;
        return { space };
      },
      delete(request) {
        const at = spaces.findIndex((candidate) => candidate.id === request.space);
        if (at >= 0) spaces.splice(at, 1);
        return {};
      },
    });

    router.service(PluginService, {
      list: () => ({ plugins }),
      get: (request) => ({ plugin: plugins.find((plugin) => plugin.id === request.id) }),
      contributions: () => ({ contributions: site.contributions ?? [] }),
      setEnabled(request) {
        const plugin = plugins.find((candidate) => candidate.id === request.id);
        if (plugin !== undefined) plugin.enabled = request.enabled;
        return { plugin };
      },
      remove(request) {
        const at = plugins.findIndex((candidate) => candidate.id === request.id);
        if (at >= 0) plugins.splice(at, 1);
        return {};
      },
      update: () => ({ plugin: plugins[0] }),
      installFromRegistry: () => ({ plugin: plugins[0] }),
      installPackage: () => ({ plugin: plugins[0] }),
    });

    router.service(RegistryService, {
      list: () => ({ registries }),
      add(request) {
        if (request.registry !== undefined) registries.push(request.registry);
        return {};
      },
      remove(request) {
        const at = registries.findIndex((registry) => registry.name === request.name);
        if (at >= 0) registries.splice(at, 1);
        return {};
      },
    });

    router.service(HostService, {
      list: () => ({ hosts: site.hosts ?? [], nextCursor: "" }),
      get: (request) => ({ host: (site.hosts ?? []).find((host) => host.name === request.name) }),
      listActions: () => ({ actions: site.actions ?? [] }),
      apply: () => ({ job: "01hjob" }),
      rename: () => ({ host: site.hosts?.[0] }),
      setLabels: (request) => {
        const host = (site.hosts ?? []).find((one) => one.name === request.name);
        if (host === undefined) return { host: undefined };
        const labels = { ...host.labels, ...request.set };
        for (const name of request.remove) delete labels[name];
        return { host: { ...host, labels } };
      },
      remove: () => ({}),
    });

    router.service(JobService, {
      list: () => ({ jobs: site.jobs ?? [], nextCursor: "" }),
      get: (request) => ({ job: (site.jobs ?? []).find((job) => job.id === request.job) }),
      follow: async function* () {
        for (const line of site.logs ?? []) yield { event: { case: "line" as const, value: line } };
        if (site.jobEnd !== undefined) yield { event: { case: "job" as const, value: site.jobEnd } };
        await forever();
      },
    });

    router.service(ProvisionerService, {
      listTypes: () => ({ types: site.types ?? [] }),
      list: () => ({ provisioners: site.provisioners ?? [], nextCursor: "" }),
      listResources: () => ({ resources: site.machines ?? [], nextCursor: "" }),
      status: () => ({ up: true, message: "up" }),
    });

    router.service(NotificationService, {
      list: () => ({ notifications, nextCursor: "" }),
      watch: async function* () {
        // The shell keeps this stream open; a test that pushes a notification
        // pushes it into `received` before rendering.
        for (const notification of pushed(site)) yield { notification };
        await forever();
      },
      markRead(request) {
        const notification = notifications.find((candidate) => candidate.id === request.notification);
        if (notification !== undefined) notification.read = request.read;
        return { notification };
      },
      answer: () => ({ notification: notifications[0] }),
      setPreference: () => ({}),
    });

    router.service(EventService, {
      subscribe: async function* () {
        for (const event of site.events ?? []) yield { event };
        await forever();
      },
    });

    router.service(UserService, {
      list: () => ({ users, nextCursor: "" }),
      search: (request) => ({
        users: users.filter((user) => user.email.includes(request.query) || user.name.includes(request.query)),
      }),
      createLink(request) {
        return {
          link: {
            $typeName: "rookery.v1.Link",
            token: "link-token",
            url: `https://rookery.test/auth/link/link-token?email=${encodeURIComponent(request.email)}`,
            expiresAt: TEST_AT,
          },
        };
      },
      setAdmin(request) {
        const user = users.find((candidate) => candidate.id === request.user);
        if (user !== undefined) user.admin = request.admin;
        return { user };
      },
      setDisabled(request) {
        const user = users.find((candidate) => candidate.id === request.user);
        if (user !== undefined) user.disabled = request.disabled;
        return { user };
      },
    });

    router.service(GroupService, {
      list: () => ({ groups, nextCursor: "" }),
      create(request) {
        const group = {
          $typeName: "rookery.v1.Group",
          id: `01hgroup${groups.length + 1}`,
          name: request.name,
          members: [],
          claims: [],
        } as Group;
        groups.push(group);
        return { group };
      },
      rename(request) {
        const group = groups.find((candidate) => candidate.id === request.group);
        if (group !== undefined) group.name = request.name;
        return { group };
      },
      delete(request) {
        const at = groups.findIndex((candidate) => candidate.id === request.group);
        if (at >= 0) groups.splice(at, 1);
        return {};
      },
      addMember(request) {
        const group = groups.find((candidate) => candidate.id === request.group);
        const user = users.find((candidate) => candidate.email === request.email);
        if (group !== undefined && user !== undefined) group.members.push(user);
        return { group };
      },
      removeMember: (request) => ({ group: groups.find((candidate) => candidate.id === request.group) }),
      bindClaim(request) {
        const group = groups.find((candidate) => candidate.id === request.group);
        if (group !== undefined) group.claims.push({ $typeName: "rookery.v1.GroupClaim", claim: request.claim, value: request.value });
        return { group };
      },
    });

    router.service(AccessService, {
      listGrants: () => ({ grants, nextCursor: "" }),
      grant(request) {
        const grant = {
          $typeName: "rookery.v1.Grant",
          resource: request.resource,
          grantee: request.grantee,
          level: request.level,
        } as Grant;
        grants.push(grant);
        return { grant };
      },
      revoke(request) {
        const at = grants.findIndex((candidate) => candidate.resource === request.resource);
        if (at >= 0) grants.splice(at, 1);
        return {};
      },
      check: () => ({ allowed: true, level: 3 }),
      level: () => ({ level: 3 }),
    });

    router.service(SettingService, {
      list: () => ({ settings }),
      get: (request) => ({ setting: settings.find((setting) => setting.key === request.key) }),
      set(request) {
        const setting = settings.find((candidate) => candidate.key === request.key);
        if (setting !== undefined) {
          setting.valueCbor = request.valueCbor;
          setting.scope = request.scope;
        }
        return { setting };
      },
      reset: () => ({}),
    });

    router.service(SecretService, {
      list: () => ({ secrets }),
      set(request) {
        const secret = {
          $typeName: "rookery.v1.Secret",
          name: request.name,
          scope: request.scope,
          createdAt: TEST_AT,
          updatedAt: TEST_AT,
        } as Secret;
        const at = secrets.findIndex((candidate) => candidate.name === request.name && candidate.scope === request.scope);
        if (at >= 0) secrets[at] = secret;
        else secrets.push(secret);
        return { secret };
      },
      delete(request) {
        const at = secrets.findIndex((candidate) => candidate.name === request.name && candidate.scope === request.scope);
        if (at >= 0) secrets.splice(at, 1);
        return {};
      },
      rename(request) {
        const secret = secrets.find((candidate) => candidate.name === request.name && candidate.scope === request.scope);
        if (secret !== undefined) secret.name = request.newName;
        return { secret };
      },
    });

    router.service(ConnectionService, {
      listTypes: () => ({ types: site.connectionTypes ?? [] }),
      list: () => ({ connections }),
      get: (request) => ({ connection: connections.find((connection) => connection.id === request.connection) }),
      create(request) {
        const connection = {
          $typeName: "rookery.v1.Connection",
          id: `01hconnection${connections.length + 1}`,
          type: request.type,
          name: request.name,
          valuesCbor: new Uint8Array(),
        } as Connection;
        connections.push(connection);
        return { connection };
      },
      update: () => ({ connection: connections[0] }),
      delete(request) {
        const at = connections.findIndex((connection) => connection.id === request.connection);
        if (at >= 0) connections.splice(at, 1);
        return {};
      },
      test: () => ({ ok: true, message: "" }),
    });

    router.service(KeyService, {
      list: () => ({ keys }),
      rotate(request) {
        for (const key of keys) key.active = false;
        const key = {
          $typeName: "rookery.v1.Key",
          id: `01hkey${keys.length + 1}`,
          name: request.name === "" ? "default" : request.name,
          type: "ssh-ed25519",
          publicKey: "ssh-ed25519 AAAA test",
          fingerprint: `SHA256:new${keys.length}`,
          active: true,
        } as Key;
        keys.push(key);
        return { key };
      },
      listKnownHosts: () => ({ hosts: knownHosts }),
      trustHostKey(request) {
        const host = knownHosts.find((candidate) => candidate.address === request.address);
        if (host !== undefined) {
          host.fingerprint = request.fingerprint;
          host.pendingFingerprint = "";
        }
        return { host };
      },
      forgetHostKey(request) {
        const at = knownHosts.findIndex((candidate) => candidate.address === request.address);
        if (at >= 0) knownHosts.splice(at, 1);
        return {};
      },
    });

    router.service(TailnetService, {
      status: () => tailnet,
      configure(request) {
        tailnet = { configured: true, online: true, controlUrl: request.controlUrl, tag: request.tag, addresses: ["100.64.0.1"] };
        return {};
      },
      listDevices: () => ({ devices: site.devices ?? [] }),
    });

    router.service(DataService, {
      listCollections: () => ({ collections: site.collections ?? [] }),
      find: (request) => ({
        recordsCbor: (records[request.collection] ?? []).map((record) => cborEncode(record)),
        cursor: "",
      }),
      get: (request) => {
        const record = (records[request.collection] ?? []).find((candidate) => candidate["id"] === request.id);
        return { recordCbor: record === undefined ? new Uint8Array() : cborEncode(record) };
      },
      history: (request) => ({ entriesCbor: site.history?.[`${request.collection}/${request.id}`] ?? [] }),
      live: async function* (request) {
        for (const record of records[request.collection] ?? []) {
          yield {
            kind: 1,
            resource: `record://${request.collection}/${String(record["id"])}`,
            recordCbor: cborEncode(record),
          };
        }
        await forever();
      },
      changes: async function* () {
        await forever();
      },
      count: () => ({ count: 0n }),
      insert: () => ({ recordCbor: new Uint8Array() }),
      update: () => ({ recordCbor: new Uint8Array() }),
      upsert: () => ({ recordCbor: new Uint8Array() }),
      delete: () => ({}),
      restore: () => ({ recordCbor: new Uint8Array() }),
      refsTo: () => ({ references: [] }),
      refsFrom: () => ({ references: [] }),
    });

    router.service(DocumentService, {
      list: () => ({ documents }),
      get: (request) => ({ document: documents.find((document) => document.id === request.id) }),
      create(request) {
        const document = {
          $typeName: "rookery.v1.Document",
          id: `01hdocument${documents.length + 1}`,
          type: request.type,
          parent: request.parent,
          owner: `user:${STATIC_USER.id}`,
          createdAt: TEST_AT,
          updatedAt: TEST_AT,
        } as Document;
        documents.push(document);
        return { document };
      },
      history: () => ({ snapshots: site.snapshots ?? [] }),
      snapshot: () => ({ at: TEST_AT }),
      delete(request) {
        const at = documents.findIndex((document) => document.id === request.id);
        if (at >= 0) documents.splice(at, 1);
        return {};
      },
      sync: async function* () {
        await forever();
      },
    });

    router.service(AuditService, {
      list: () => ({ entries: site.audit ?? [], nextCursor: "" }),
    });
  });
}

/** cborEncode is the client's own encoder, so a test's record round-trips. */
function cborEncode(value: unknown): Uint8Array {
  return CBOR.encode(value);
}

/** pushed is what a test queued for the notification stream. */
function pushed(site: FakeSite): Notification[] {
  const queued = site.notifications ?? [];
  return queued.slice(1);
}

/** forever keeps a stream open, as the edge does while the page is open. */
function forever(): Promise<never> {
  return new Promise<never>(() => undefined);
}

/** fakeClient is the shell's client on a made-up server. */
export function fakeClient(site: FakeSite, halves: readonly LoadedClientHalf[] = []): Rookery {
  const client = createRookery({
    baseUrl: "https://rookery.test",
    transport: fakeTransport(site),
    autoSignIn: false,
  });
  // A browser imports a client half from `/plugins/...`, which a test cannot
  // reach; the halves it wants are handed to the loader instead.
  return Object.assign(client, {
    loadClientHalves: async () => halves,
  }) as Rookery;
}

import { cbor as CBOR } from "@rookery/client";

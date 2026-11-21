// The edge, as a browser talks to it: Connect over HTTP with the session cookie,
// the CSRF token in a header, the current space in another, and one typed client
// per service.
//
// A new core service joins `services` below with one line when its proto lands;
// everything else follows from the generated service descriptor.
import {
  AccessService,
  AuditService,
  AuthService,
  ChannelService,
  ConnectionService,
  DataService,
  DesktopService,
  DocumentService,
  EntityService,
  EventService,
  ExecService,
  FileService,
  GroupService,
  HostCallService,
  HostService,
  JobService,
  KeyService,
  NotificationService,
  PluginService,
  ProvisionerService,
  RegistryService,
  RolloutService,
  ScheduleService,
  SecretService,
  SettingService,
  SpaceService,
  TailnetService,
  TerminalService,
  TopicService,
  UserService,
} from "@rookery/proto";
import {
  type Client,
  Code,
  ConnectError,
  type Transport,
  createClient,
} from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";

import { CSRF_COOKIE, CSRF_HEADER, SPACE_HEADER, readCookie } from "./cookies.ts";
import type { PluginContributions } from "./contributions.ts";
import { fetchContributions } from "./contributions.ts";
import { DocumentSync, type DocumentOptions } from "./documents.ts";
import { DesktopStream, type DesktopOptions, desktopUrl } from "./desktop.ts";
import type { RookeryEndpoints } from "./endpoints.ts";
import { type LoadedClientHalf, type LoaderOptions, loadClientHalves } from "./loader.ts";
import { type SignIn, createSignIn } from "./session.ts";
import { TerminalSession, type TerminalOptions } from "./terminals.ts";

/**
 * services is every core service the browser may call, by the name the client
 * exposes it under. The proto's descriptor carries the procedure names, so
 * nothing here names an RPC.
 */
export const services = {
  access: AccessService,
  audit: AuditService,
  auth: AuthService,
  channels: ChannelService,
  connections: ConnectionService,
  data: DataService,
  documents: DocumentService,
  entities: EntityService,
  events: EventService,
  exec: ExecService,
  files: FileService,
  groups: GroupService,
  hostCalls: HostCallService,
  hosts: HostService,
  jobs: JobService,
  keys: KeyService,
  notifications: NotificationService,
  plugins: PluginService,
  provisioners: ProvisionerService,
  registries: RegistryService,
  rollouts: RolloutService,
  schedules: ScheduleService,
  secrets: SecretService,
  settings: SettingService,
  spaces: SpaceService,
  tailnet: TailnetService,
  terminals: TerminalService,
  topics: TopicService,
  users: UserService,
} as const;

/** ServiceName is one of the names in `services`. */
export type ServiceName = keyof typeof services;

/** Clients is one typed client per core service. */
export type Clients = { readonly [K in ServiceName]: Client<(typeof services)[K]> };

/** RookeryOptions configures a client for one server. */
export interface RookeryOptions {
  /**
   * baseUrl is the edge's address, such as "https://rookery.example.com". It
   * defaults to the page's own origin, which is where the shell is served.
   */
  baseUrl?: string;
  /** fetch overrides the platform's fetch, for tests and unusual hosts. */
  fetch?: typeof fetch;
  /** transport overrides the whole Connect transport, for tests. */
  transport?: Transport;
  /**
   * csrf returns the session's CSRF token. It defaults to the edge's cookie,
   * which a page may read; a page that keeps the token elsewhere passes its own.
   */
  csrf?: () => string | undefined;
  /** credentials is what a call sends cookies with: "include" by default. */
  credentials?: RequestCredentials;
  /** space is the space the client starts in. */
  space?: string;
  /**
   * autoSignIn asks AuthService.Me as soon as the client is made, so that a
   * page knows who it is before it renders. It defaults to true.
   */
  autoSignIn?: boolean;
}

/** Rookery is the edge's typed API, and what a client half builds on. */
export type Rookery = Clients &
  RookeryEndpoints & {
    /** transport is the Connect transport every client shares. */
    readonly transport: Transport;
    /** signIn is who the caller is, and the calls that change it. */
    readonly signIn: SignIn;
    /** setSpace changes the space every following call is scoped to. */
    setSpace(space: string): void;
    /**
     * inSpace is a client whose calls are always in this space, or in none for
     * "", whatever setSpace says. It shares the sign-in, so a page that shows
     * one space beside a page in another keeps each one's calls in its own.
     */
    inSpace(space: string): Rookery;
    /** documents is DocumentService, with the live sync over `/ws/documents/{id}`. */
    readonly documents: Client<typeof DocumentService> & {
      open(id: string, options?: DocumentOptions): DocumentSync;
    };
    /** terminals is TerminalService, with the attach over `/ws/terminals/{id}`. */
    readonly terminals: Client<typeof TerminalService> & {
      attach(id: string, options?: TerminalOptions): TerminalSession;
    };
    /** desktop is a host's RFB stream over `/ws/hosts/{name}/desktop`. */
    readonly desktop: Omit<Client<typeof DesktopService>, "connect"> & {
      url(host: string): string;
      open(host: string, options?: DesktopOptions): DesktopStream;
    };
    /** contributions is what the installed plugins add to the shell. */
    contributions(): Promise<readonly PluginContributions[]>;
    /** loadClientHalves imports every installed plugin's client half. */
    loadClientHalves(options?: LoaderOptions): Promise<readonly LoadedClientHalf[]>;
  };

/** createRookery makes the client for one server. */
export function createRookery(options: RookeryOptions = {}): Rookery {
  return build(options);
}

/** build makes a client, sharing the sign-in of the one it is scoped from. */
function build(options: RookeryOptions, parent?: Rookery): Rookery {
  const baseUrl = (options.baseUrl ?? originOf()).replace(/\/+$/, "");
  const csrf = options.csrf ?? (() => readCookie(CSRF_COOKIE));
  let space = options.space ?? "";

  const transport =
    options.transport ??
    createConnectTransport({
      baseUrl,
      // Connect's binary encoding, which is smaller than its JSON and what the
      // edge answers with.
      useBinaryFormat: true,
      // The CSRF token and the space go on every HTTP call, streams included,
      // which is why they are set on the fetch rather than in an interceptor.
      fetch: edgeFetch(options.fetch ?? globalThis.fetch, {
        credentials: options.credentials ?? "include",
        headers: () => ({
          [CSRF_HEADER]: csrf(),
          [SPACE_HEADER]: space === "" ? undefined : space,
        }),
      }),
    });

  const clients: Clients = {} as Clients;
  const made = clients as unknown as Record<ServiceName, Client<never>>;
  for (const [name, service] of Object.entries(services)) {
    made[name as ServiceName] = createClient(service, transport) as Client<never>;
  }

  const { screenshot, input, getClipboard, setClipboard, pause, resume } = createClient(DesktopService, transport);
  const scoped = new Map<string, Rookery>();
  const rookery: Rookery = {
    ...(clients as unknown as Clients),
    baseUrl,
    transport,
    signIn: parent?.signIn ?? createSignIn({
      me: async () => {
        const { user } = await clients.auth.me({});
        if (user === undefined) {
          throw new ConnectError("AuthService.Me answered without a user", Code.Internal);
        }
        return user;
      },
      signOut: async () => {
        const response = await (options.fetch ?? globalThis.fetch)(
          `${baseUrl}/auth/logout`,
          {
            method: "POST",
            credentials: options.credentials ?? "include",
            headers: withToken({}, csrf()),
          },
        );
        if (!response.ok) {
          throw new Error(`signing out: ${response.status} ${response.statusText}`);
        }
      },
    }),
    space: () => space,
    // A client made by inSpace stays in its space.
    setSpace: (next) => {
      if (parent === undefined) space = next;
    },
    inSpace: (id) => {
      if (parent !== undefined) return parent.inSpace(id);
      let found = scoped.get(id);
      if (found === undefined) {
        found = build({ ...options, space: id, autoSignIn: false }, rookery);
        scoped.set(id, found);
      }
      return found;
    },
    url: (path) => `${baseUrl}${path.startsWith("/") ? path : `/${path}`}`,
    webSocketUrl: (path, query) => {
      const url = new URL(rookery.url(path));
      url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
      if (space !== "") url.searchParams.set("space", space);
      for (const [key, value] of Object.entries(query ?? {})) {
        if (value !== undefined) url.searchParams.set(key, value);
      }
      return url.toString();
    },
    documents: Object.assign(clients.documents, {
      open: (id: string, documentOptions?: DocumentOptions) =>
        new DocumentSync(rookery, id, documentOptions),
    }),
    terminals: Object.assign(clients.terminals, {
      attach: (id: string, terminalOptions?: TerminalOptions) =>
        new TerminalSession(rookery, id, terminalOptions),
    }),
    desktop: {
      screenshot,
      input,
      getClipboard,
      setClipboard,
      pause,
      resume,
      url: (host) => desktopUrl(rookery, host),
      open: (host, desktopOptions) =>
        new DesktopStream(rookery, host, desktopOptions as DesktopOptions | undefined).open(),
    },
    contributions: () => fetchContributions(transport),
    loadClientHalves: (loaderOptions) =>
      loadClientHalves(rookery, { source: () => fetchContributions(transport), ...loaderOptions }),
  };

  if (parent === undefined && (options.autoSignIn ?? true)) {
    void rookery.signIn.refresh().catch(() => undefined);
  }
  return rookery;
}

/** originOf is the page's origin, or the empty string where there is none. */
function originOf(): string {
  return typeof location === "undefined" ? "" : location.origin;
}

/**
 * edgeFetch is the platform's fetch with what every call to the edge needs:
 * cookies, and the headers that are not the call's own.
 */
function edgeFetch(
  fetchImpl: typeof fetch,
  options: {
    credentials: RequestCredentials;
    headers: () => Record<string, string | undefined>;
  },
): typeof fetch {
  return ((input: RequestInfo | URL, init?: RequestInit) => {
    const headers = new Headers(init?.headers);
    for (const [name, value] of Object.entries(options.headers())) {
      if (value !== undefined && value !== "") headers.set(name, value);
    }
    return fetchImpl(input, { ...init, headers, credentials: options.credentials });
  }) as typeof fetch;
}

/** withToken adds the CSRF header to a plain HTTP call, such as sign-out. */
function withToken(headers: Record<string, string>, token: string | undefined): HeadersInit {
  if (token !== undefined && token !== "") headers[CSRF_HEADER] = token;
  return headers;
}

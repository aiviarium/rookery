// A server the shell's tests talk to, without a network: a Connect router that
// answers each service from a made-up site.
//
// Every call a page makes goes through the client's own transport, so the tests
// exercise the same path a browser does, with the answers a test sets.
import {
  type Transport,
  Code,
  ConnectError,
  createRouterTransport,
} from "@connectrpc/connect";
import { type LoadedClientHalf, type Rookery, createRookery } from "@rookery/client";
import {
  AuthService,
  EventService,
  HostService,
  JobService,
  NotificationService,
  PluginService,
  ProvisionerService,
  SpaceService,
} from "@rookery/proto";
import type {
  Action,
  Event,
  Host,
  Job,
  JobLine,
  Notification,
  Plugin,
  PluginContribution,
  ProvisionerEntry,
  ProvisionerResource,
  ProvisionerType,
  Space,
  User,
} from "@rookery/proto";

/** FakeSite is what the made-up server holds. */
export interface FakeSite {
  /** user is who is signed in, or undefined for nobody. */
  user?: User;
  spaces?: Space[];
  plugins?: Plugin[];
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
  contributions?: PluginContribution[];
  /** events are sent on the event stream, one at a time. */
  events?: Event[];
}

/** STATIC_USER is the person the tests sign in. */
export const STATIC_USER: User = {
  id: "01huser",
  email: "ada@example.test",
  name: "Ada Lovelace",
  admin: true,
  disabled: false,
} as User;

/** A_SPACE and ANOTHER_SPACE are the two spaces the tests may switch between. */
export const A_SPACE: Space = {
  id: "01hspace",
  kind: "team",
  name: "Analysis",
  archived: false,
  createdBy: "01huser",
} as Space;
export const ANOTHER_SPACE: Space = {
  id: "01hspace2",
  kind: "team",
  name: "Engines",
  archived: false,
  createdBy: "01huser",
} as Space;

/** fakeTransport answers the services the shell calls from a site. */
export function fakeTransport(site: FakeSite): Transport {
  let notifications = [...(site.notifications ?? [])];

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
      listKinds: () => ({ kinds: [{ id: "team", label: "Team" }] }),
      list: () => ({ spaces: site.spaces ?? [], nextCursor: "" }),
      get: (request) => ({ space: (site.spaces ?? []).find((space) => space.id === request.space) }),
      create: () => ({ space: A_SPACE }),
      rename: () => ({ space: A_SPACE }),
      archive: () => ({ space: A_SPACE }),
      delete: () => ({}),
    });

    router.service(PluginService, {
      list: () => ({ plugins: site.plugins ?? [] }),
      get: (request) => ({ plugin: (site.plugins ?? []).find((plugin) => plugin.id === request.id) }),
      contributions: () => ({ contributions: site.contributions ?? [] }),
      setEnabled: () => ({ plugin: site.plugins?.[0] }),
      remove: () => ({}),
      update: () => ({ plugin: site.plugins?.[0] }),
      installFromRegistry: () => ({ plugin: site.plugins?.[0] }),
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
        notifications = notifications.map((notification) =>
          notification.id === request.notification ? { ...notification, read: request.read } : notification,
        );
        const notification = notifications.find((candidate) => candidate.id === request.notification);
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
  });
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

// The installed plugins' client halves: their pages, their navigation, their
// host tabs and the widgets they add.
//
// The edge says what is installed, and each half is imported from its immutable
// route. A half that fails to load is reported and left out, so one broken
// plugin does not keep the shell from starting.
import { type Accessor, createEffect, createSignal } from "solid-js";
import {
  type LoadedClientHalf,
  type PluginContributions,
  type Rookery,
  clientContext,
  isPermissionDenied,
  isUnauthenticated,
} from "@rookery/client";

/** PluginMount is one page a client half draws. */
export interface PluginMount {
  /** plugin is the plugin's id. */
  readonly plugin: string;
  /** kind is which of the half's maps the mount is in. */
  readonly kind: "views" | "adminPages";
  /** id is the mount's id inside that map. */
  readonly id: string;
  /** label is what the shell calls the page. */
  readonly label: string;
  /** route is the path the page answers. */
  readonly route: string;
  /** space is the kind of space the page needs, when it needs one. */
  readonly space?: string;
  /** half is the plugin whose client half draws it. */
  readonly half: LoadedClientHalf;
}

/** PluginNav is one entry a client half adds to the shell's navigation. */
export interface PluginNav {
  readonly label: string;
  readonly href: string;
  readonly group: string;
}

/** HostTab is a tab a client half adds to a host's page. */
export interface HostTab {
  readonly plugin: string;
  readonly id: string;
  readonly label: string;
  /** types are the host types the tab is for, empty for every type. */
  readonly types: readonly string[];
  readonly half: LoadedClientHalf;
}

/** Plugins is what the installed plugins add, and whether it loaded. */
export interface Plugins {
  /** contributions is what the edge said the plugins add. */
  readonly contributions: Accessor<readonly PluginContributions[]>;
  /** halves are the client halves that loaded. */
  readonly halves: Accessor<readonly LoadedClientHalf[]>;
  /** mounts are every page they draw, the shell's own pages aside. */
  readonly mounts: Accessor<readonly PluginMount[]>;
  /** nav is what they add to the navigation. */
  readonly nav: Accessor<readonly PluginNav[]>;
  /** hostTabs are the tabs they add to a host's page. */
  readonly hostTabs: Accessor<readonly HostTab[]>;
  /** loading says whether the halves are being loaded. */
  readonly loading: Accessor<boolean>;
  /** error is why they could not be loaded. */
  readonly error: Accessor<Error | undefined>;
  /** reload asks the edge again, as it does after a plugin is installed. */
  reload(): Promise<void>;
}

/** DEFAULT_GROUP is where a view without a navigation group sits. */
const DEFAULT_GROUP = "Plugins";

/** createPlugins loads the enabled plugins' client halves. */
export function createPlugins(
  rookery: Rookery,
  signedIn: Accessor<boolean>,
  spaceKind: Accessor<string | undefined>,
): Plugins {
  const [contributions, setContributions] = createSignal<readonly PluginContributions[]>([]);
  const [halves, setHalves] = createSignal<readonly LoadedClientHalf[]>([]);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal<Error>();
  const [generation, setGeneration] = createSignal(0);

  const load = async () => {
    const mine = generation() + 1;
    setGeneration(mine);
    setLoading(true);
    try {
      const declared = await rookery.contributions();
      // The plugin whose client half is the shell is not a half the shell
      // hosts: it is the shell, and this shell would not be served.
      const hosted = declared.filter((entry) => entry.shell !== true && entry.hash !== "");
      const loaded = await rookery.loadClientHalves({ contributions: hosted });
      if (mine !== generation()) return;
      setContributions(hosted);
      setHalves(loaded);
      setError(undefined);
    } catch (failure) {
      if (mine !== generation()) return;
      // A signed-out or refused caller has no halves, which is not a fault of
      // the plugins.
      if (!isUnauthenticated(failure) && !isPermissionDenied(failure)) setError(asError(failure));
    } finally {
      if (mine === generation()) setLoading(false);
    }
  };

  createEffect(
    () => signedIn(),
    (signed) => {
      if (signed) {
        void load();
      } else {
        setContributions([]);
        setHalves([]);
        setError(undefined);
      }
    },
  );

  const mounts = (): readonly PluginMount[] => {
    const out: PluginMount[] = [];
    for (const half of halves()) {
      for (const view of half.contributes.views ?? []) {
        out.push({ plugin: half.plugin, kind: "views", id: view.id, label: view.label, route: view.route, space: view.space, half });
      }
      for (const page of half.contributes.adminPages ?? []) {
        out.push({ plugin: half.plugin, kind: "adminPages", id: page.id, label: page.label, route: page.route, half });
      }
    }
    return out;
  };

  const nav = (): readonly PluginNav[] => {
    const out: PluginNav[] = [];
    for (const half of halves()) {
      for (const entry of half.contributes.nav ?? []) {
        const view = (half.contributes.views ?? []).find((candidate) => candidate.id === entry.view);
        if (view === undefined) continue;
        // A view that needs a kind of space is offered where there is one.
        if (view.space !== undefined && view.space !== "" && view.space !== spaceKind()) continue;
        out.push({ label: view.label, href: view.route, group: entry.group ?? DEFAULT_GROUP });
      }
    }
    return out;
  };

  const hostTabs = (): readonly HostTab[] => {
    const out: HostTab[] = [];
    for (const half of halves()) {
      for (const tab of half.contributes.hostTabs ?? []) {
        out.push({ plugin: half.plugin, id: tab.id, label: tab.label, types: tab.types ?? [], half });
      }
    }
    return out;
  };

  return {
    contributions,
    halves,
    mounts,
    nav,
    hostTabs,
    loading,
    error,
    reload: load,
  };
}

/**
 * activate starts every loaded half, once, with the context it mounts with. A
 * half that starts twice would register its listeners twice.
 */
export function activateHalves(
  rookery: Rookery,
  halves: readonly LoadedClientHalf[],
  navigate: (path: string) => void,
  started: Set<string>,
): void {
  for (const half of halves) {
    const key = `${half.plugin}/${half.hash}`;
    if (started.has(key)) continue;
    started.add(key);
    const context = clientContext(rookery, half, navigate);
    if (half.half.start === undefined) continue;
    try {
      void Promise.resolve(half.half.start(context)).catch(() => undefined);
    } catch {
      // A half that fails to start keeps the shell working; its pages say what
      // they can.
    }
  }
}

/** asError is anything thrown, as an Error. */
function asError(failure: unknown): Error {
  return failure instanceof Error ? failure : new Error(String(failure));
}

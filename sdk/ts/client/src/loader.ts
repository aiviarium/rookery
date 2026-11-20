// The client-half loader: ask what the installed plugins contribute, import each
// one's `index.js` from `/plugins/{id}/{hash}/`, and hand the shell the halves
// with the context they mount with.
//
// The route is immutable and cacheable — the hash is part of it — so a plugin
// update is a new URL and a normal module cache does the rest.
import type { ClientContext, ClientHalf } from "./contract.ts";
import type { Contributions, PluginContributions } from "./contributions.ts";
import type { RookeryEndpoints } from "./endpoints.ts";
import type { Rookery } from "./transport.ts";

/** clientHalfUrl is a plugin's client half, as `/plugins/{id}/{hash}/index.js`. */
export function clientHalfUrl(endpoints: RookeryEndpoints, entry: PluginContributions): string {
  return endpoints.url(
    `/plugins/${encodeURIComponent(entry.plugin)}/${encodeURIComponent(entry.hash)}/index.js`,
  );
}

/** LoadedClientHalf is one client half, imported and ready to mount. */
export interface LoadedClientHalf {
  /** plugin is the plugin's id. */
  readonly plugin: string;
  /** hash is the client half's content hash. */
  readonly hash: string;
  /** shell is set for the shell plugin. */
  readonly shell: boolean;
  /** contributes is what the plugin declared. */
  readonly contributes: Contributions;
  /** half is what its `index.js` exported. */
  readonly half: ClientHalf;
  /** url is where it came from. */
  readonly url: string;
}

/** LoaderOptions configures the loader. */
export interface LoaderOptions {
  /**
   * importModule imports a client half's module. It defaults to the platform's
   * dynamic import, which a browser needs and a test replaces.
   */
  importModule?: (url: string) => Promise<unknown>;
  /** contributions overrides the contributions call, as a shell already given them does. */
  contributions?: readonly PluginContributions[];
  /** source asks the edge what the plugins add, when contributions are not given. */
  source?: () => Promise<readonly PluginContributions[]>;
  /** enabled skips a plugin, such as one the user turned off. */
  enabled?: (entry: PluginContributions) => boolean;
  /** onError hears a client half that failed to load, and it is left out. */
  onError?: (error: Error, entry: PluginContributions) => void;
}

/**
 * loadClientHalves fetches the contributions and imports every client half that
 * has one. A half that fails to load is reported and left out, so one broken
 * plugin does not keep the shell from starting.
 */
export async function loadClientHalves(
  endpoints: RookeryEndpoints,
  options: LoaderOptions = {},
): Promise<readonly LoadedClientHalf[]> {
  const entries = options.contributions ?? (await options.source?.()) ?? [];
  const importModule: (url: string) => Promise<unknown> =
    options.importModule ?? ((url) => import(/* @vite-ignore */ url) as Promise<unknown>);

  const loaded: LoadedClientHalf[] = [];
  for (const entry of entries) {
    if (options.enabled !== undefined && !options.enabled(entry)) continue;
    if (entry.hash === "" || entry.hash === undefined) continue;
    const url = clientHalfUrl(endpoints, entry);
    try {
      const module = (await importModule(url)) as {
        default?: unknown;
        client?: unknown;
      };
      const half = (module.default ?? module.client) as ClientHalf | undefined;
      if (!isClientHalf(half)) {
        throw new Error(`${url} does not default-export a client half`);
      }
      loaded.push({
        plugin: entry.plugin,
        hash: entry.hash,
        shell: entry.shell ?? false,
        contributes: entry.contributes ?? {},
        half,
        url,
      });
    } catch (error) {
      options.onError?.(error instanceof Error ? error : new Error(String(error)), entry);
    }
  }
  return loaded;
}

/**
 * clientContext builds the context a client half mounts with: the client's API,
 * the plugin's own id and contributions, and the shell's navigation.
 */
export function clientContext(
  rookery: Rookery,
  half: LoadedClientHalf,
  navigate: (path: string) => void = (path) => {
    if (typeof location !== "undefined") location.assign(path);
  },
): ClientContext {
  return {
    rookery,
    plugin: { id: half.plugin },
    contributes: half.contributes,
    navigate,
  };
}

/** isClientHalf reports whether a module's export is a client half. */
function isClientHalf(value: unknown): value is ClientHalf {
  if (typeof value !== "object" || value === null) return false;
  for (const key of ["views", "widgets", "hostTabs", "adminPages"]) {
    const mounts = (value as Record<string, unknown>)[key];
    if (mounts === undefined) continue;
    if (typeof mounts !== "object" || mounts === null) return false;
    for (const mount of Object.values(mounts)) {
      if (typeof mount !== "function") return false;
    }
  }
  return true;
}

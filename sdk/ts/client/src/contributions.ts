// What an installed plugin adds to the shell: its client half's folder hash and
// the views, tabs and pages it declares.
//
// These types mirror `Contributions` in `internal/plugins/manifest.go`, which is
// what `PluginService.Contributions` returns. That proto is not on `go` yet, so
// the call below speaks Connect's JSON encoding against the shape below; when
// the proto lands, the response is its generated message and this file keeps the
// same names for the shell.
import { Code, ConnectError } from "@connectrpc/connect";

import type { RookeryEndpoints } from "./endpoints.ts";

/** View is a page a client half adds. */
export interface View {
  readonly id: string;
  readonly label: string;
  /** route is the path the shell shows it at. */
  readonly route: string;
  /** space is the kind of space it needs, if any. */
  readonly space?: string;
}

/** Nav is an entry in the shell's navigation. */
export interface Nav {
  readonly view: string;
  /** group is the section it sits in. */
  readonly group?: string;
}

/** HostTab is a tab on a host's page. */
export interface HostTab {
  readonly id: string;
  readonly label: string;
  readonly types?: readonly string[];
}

/** Widget is a tile a client half may show. */
export interface Widget {
  readonly id: string;
  readonly label?: string;
  /** view is the view it shows, when it shows one. */
  readonly view?: string;
}

/** AdminPage is an administrative page a client half adds. */
export interface AdminPage {
  readonly id: string;
  readonly label: string;
  readonly route: string;
}

/** Contributions is what a client half adds to the shell. */
export interface Contributions {
  readonly views?: readonly View[];
  readonly nav?: readonly Nav[];
  readonly hostTabs?: readonly HostTab[];
  readonly widgets?: readonly Widget[];
  readonly adminPages?: readonly AdminPage[];
}

/** PluginContributions is one installed plugin's contributions, and its client half. */
export interface PluginContributions {
  /** plugin is the plugin's id. */
  readonly plugin: string;
  /** version is the installed version. */
  readonly version?: string;
  /** hash is the client half's content hash, which `/plugins/{id}/{hash}/` names. */
  readonly hash: string;
  /** shell is set for the one plugin whose page Rookery serves for `/`. */
  readonly shell?: boolean;
  /** contributes is what its client half adds, empty for a plugin without one. */
  readonly contributes?: Contributions;
}

/** ContributionsResponse is PluginService.Contributions' answer. */
export interface ContributionsResponse {
  readonly plugins?: readonly PluginContributions[];
}

/**
 * fetchContributions asks the edge what the installed plugins add. A deployment
 * with no plugins answers with none.
 */
export async function fetchContributions(
  endpoints: RookeryEndpoints,
  options: { fetch?: typeof fetch; credentials?: RequestCredentials } = {},
): Promise<readonly PluginContributions[]> {
  const fetchImpl = options.fetch ?? globalThis.fetch;
  const response = await fetchImpl(
    endpoints.url("/rookery.v1.PluginService/Contributions"),
    {
      method: "POST",
      credentials: options.credentials ?? "include",
      headers: {
        "Content-Type": "application/json",
        "Connect-Protocol-Version": "1",
      },
      body: "{}",
    },
  );
  if (!response.ok) {
    throw new ConnectError(
      `asking for the plugins' contributions: ${response.status} ${response.statusText}`,
      connectCode(response.status),
    );
  }
  const answer = (await response.json()) as ContributionsResponse;
  return answer.plugins ?? [];
}

/** connectCode maps an HTTP status to the Connect code that means it. */
function connectCode(status: number): Code {
  switch (status) {
    case 400:
      return Code.InvalidArgument;
    case 401:
      return Code.Unauthenticated;
    case 403:
      return Code.PermissionDenied;
    case 404:
      return Code.Unimplemented;
    case 409:
      return Code.Aborted;
    case 429:
      return Code.ResourceExhausted;
    case 502:
    case 503:
      return Code.Unavailable;
    case 504:
      return Code.DeadlineExceeded;
    default:
      return Code.Internal;
  }
}

// What an installed plugin adds to the shell: its client half's folder hash and
// the views, tabs and pages it declares.
//
// The types mirror `Contributions` in `internal/plugins/manifest.go`. The edge
// sends the manifest's `contributes` block as JSON bytes, which is what
// `PluginService.Contributions` carries.
import { type Transport, createClient } from "@connectrpc/connect";

import { PluginService } from "@rookery/proto";

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
  /** hash is the client half's content hash, which `/plugins/{id}/{hash}/` names. */
  readonly hash: string;
  /** shell is set for the one plugin whose page Rookery serves for `/`. */
  readonly shell?: boolean;
  /** contributes is what its client half adds, empty for a plugin without one. */
  readonly contributes?: Contributions;
}

/**
 * fetchContributions asks the edge what the enabled plugins add. A deployment
 * with no plugins answers with none.
 */
export async function fetchContributions(
  transport: Transport,
): Promise<readonly PluginContributions[]> {
  const answer = await createClient(PluginService, transport).contributions({});
  return answer.contributions.map((contribution) => ({
    plugin: contribution.plugin,
    hash: contribution.hash,
    shell: contribution.shell,
    contributes: decodeContributions(contribution.contributesJson),
  }));
}

/** decodeContributions reads the manifest's `contributes` block. */
function decodeContributions(json: Uint8Array): Contributions {
  if (json.length === 0) return {};
  return JSON.parse(new TextDecoder().decode(json)) as Contributions;
}

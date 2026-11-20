// What a client half declares, and what the shell mounts.
//
// A client half is a browser module: `web/index.js` in the plugin's package, as
// its manifest's `client` says. It default-exports `defineClient({...})`, and the
// shell gives each view, widget, host tab and admin page the context
// `@rookery/client` describes.

import type { ClientHalf } from "@rookery/client";

export type {
  ClientContext,
  ClientHalf,
  HostTabMount,
  Mount,
} from "@rookery/client";

/**
 * defineClient is a client half's declaration.
 *
 * It is the identity function with a type: writing it says what the module is,
 * and it is what the shell's loader checks for a `views`, `widgets`, `hostTabs`
 * and `adminPages` map. A mount's key is the id the manifest declares, so a
 * typo is a key the shell reports rather than a page that draws nothing.
 */
export function defineClient(half: ClientHalf): ClientHalf {
  return half;
}

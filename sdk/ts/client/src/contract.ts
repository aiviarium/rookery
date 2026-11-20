// The contract between Rookery and a client half: what `defineClient` declares in
// `@rookery/sdk/client`, and what the shell mounts.
//
// The context is the same for every client half: the typed API, who is signed in,
// the current space, live queries, documents, terminals and the desktop, and the
// shell's navigation. A client half never builds a transport of its own.
import type { Contributions } from "./contributions.ts";
import type { Rookery } from "./transport.ts";

/** ClientContext is what the shell gives a client half. */
export interface ClientContext {
  /** rookery is the edge: one typed client per service, sign-in and the space. */
  readonly rookery: Rookery;
  /** plugin is the client half's own plugin. */
  readonly plugin: { readonly id: string };
  /** contributes is what this plugin declared in its manifest. */
  readonly contributes: Contributions;
  /** navigate sends the shell to a path, such as a view's route. */
  navigate(path: string): void;
}

/** Mount draws something into an element, and returns what removes it. */
export type Mount = (context: ClientContext, element: HTMLElement) => void | (() => void);

/** HostTabMount draws a host's tab, with the host it is about. */
export type HostTabMount = (
  context: ClientContext,
  element: HTMLElement,
  host: string,
) => void | (() => void);

/**
 * ClientHalf is what a client half's `index.js` default-exports: one mount per id
 * its manifest declares. An id without a mount is a page the shell draws nothing
 * on, which is a manifest mistake the shell states.
 */
export interface ClientHalf {
  readonly views?: Readonly<Record<string, Mount>>;
  readonly widgets?: Readonly<Record<string, Mount>>;
  readonly hostTabs?: Readonly<Record<string, HostTabMount>>;
  readonly adminPages?: Readonly<Record<string, Mount>>;
  /** start runs once, when the client half is loaded. */
  start?(context: ClientContext): void | Promise<void>;
  /** stop runs when the shell unloads the client half. */
  stop?(context: ClientContext): void;
}

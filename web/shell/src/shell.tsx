// The shell's state: the client, who is signed in, the space it is in, the
// connection, the notifications and the plugins' client halves.
//
// Pages read it with `useShell()`. They never build a transport of their own:
// everything goes through the one client the shell made.
import { type Accessor, createContext, createEffect, createSignal, useContext } from "solid-js";
import {
  type AuthConfig,
  type Rookery,
  createRookery,
  fetchAuthConfig,
} from "@rookery/client";
import { type SignInAccessor, useSignIn } from "@rookery/client/solid";

import { type Connection, createConnection } from "./connection.ts";
import { type Notifications, createNotifications } from "./notifications.ts";
import { type Plugins, createPlugins } from "./plugins.ts";
import { type Spaces, createSpaces } from "./spaces.ts";
import { type Theme, createTheme } from "./theme.ts";

/** Shell is what every page and every plugin mounts with. */
export interface Shell {
  /** rookery is the edge. */
  readonly rookery: Rookery;
  /** signIn is who is signed in, as a signal. */
  readonly signIn: SignInAccessor;
  /** config is which ways into this deployment there are. */
  readonly config: Accessor<AuthConfig>;
  /** spaces is the space list, and the space the address is in. */
  readonly spaces: Spaces;
  /** connection is the link to the server, which the status bar shows. */
  readonly connection: Connection;
  /** notifications is the person's own. */
  readonly notifications: Notifications;
  /** plugins are the installed client halves. */
  readonly plugins: Plugins;
  /** theme is the person's choice of dark, light or the system's. */
  readonly theme: Theme;
  /** navigate sends the shell to a path, through its router. */
  navigate(path: string): void;
  /** setRouterNavigation installs the router's navigate, and removes it on undefined. */
  setRouterNavigation(navigate?: (path: string) => void): void;
}

/** ShellContext is the shell, for the components under it. */
export const ShellContext = createContext<Shell>();

/** useShell is the shell every page draws from. */
export function useShell(): Shell {
  return useContext(ShellContext);
}

/** ShellOptions are what a shell needs beyond the page's own origin. */
export interface ShellOptions {
  /** rookery overrides the client, for a page served elsewhere and for tests. */
  rookery?: Rookery;
  /** config overrides the sign-in configuration, for tests. */
  config?: AuthConfig;
}

/** createShell builds the shell's state. Call it in a component's body. */
export function createShell(options: ShellOptions = {}): Shell {
  const rookery = options.rookery ?? createRookery();
  const [config, setConfig] = createSignal<AuthConfig>(options.config ?? { identityProvider: false });
  if (options.config === undefined) {
    void fetchAuthConfig(rookery.baseUrl).then(setConfig).catch(() => undefined);
  }

  const signIn = useSignIn(rookery);
  const signedIn = () => signIn().status === "signed-in";
  const spaces = createSpaces(rookery);
  const connection = createConnection(rookery, signedIn);
  const notifications = createNotifications(rookery, signedIn);
  const plugins = createPlugins(rookery, signedIn);
  const theme = createTheme();

  // The space list is one call per sign-in.
  createEffect(
    () => signedIn(),
    (signed) => {
      if (signed) void spaces.refresh();
    },
  );

  let routerNavigate: ((path: string) => void) | undefined;
  return {
    rookery,
    signIn,
    config,
    spaces,
    connection,
    notifications,
    plugins,
    theme,
    navigate: (path) => {
      if (routerNavigate !== undefined) {
        routerNavigate(path);
        return;
      }
      if (typeof location !== "undefined") location.assign(path);
    },
    setRouterNavigation: (navigate) => {
      routerNavigate = navigate;
    },
  };
}

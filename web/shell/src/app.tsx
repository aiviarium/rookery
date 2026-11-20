// The shell's application: the state, the sign-in gate and the router.
import { Match, Switch } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Skeleton } from "@aiviarium/nocturne";
import type { AuthConfig, Rookery } from "@rookery/client";

import { ShellLayout } from "./chrome/layout.tsx";
import { Router } from "./router.ts";
import { ShellContext, createShell } from "./shell.tsx";
import { SignIn } from "./sign-in.tsx";

/** AppProps are what a page needs beyond its own origin, and tests provide. */
export interface AppProps {
  /** client overrides the edge's client, for tests. */
  readonly client?: Rookery;
  /** config overrides the sign-in configuration, for tests. */
  readonly config?: AuthConfig;
}

/** App is the whole shell. */
export function App(props: AppProps): JSX.Element {
  const shell = createShell({ rookery: props.client, config: props.config });

  return (
    <ShellContext value={shell}>
      <Switch>
        <Match when={shell.signIn().status === "unknown"}>
          <div class="shell-centered">
            <Skeleton width="160px" height="14px" />
          </div>
        </Match>
        <Match when={shell.signIn().status === "signed-in"}>
          <Router>{content => <ShellLayout>{content.children}</ShellLayout>}</Router>
        </Match>
        <Match when={true}>
          <SignIn />
        </Match>
      </Switch>
    </ShellContext>
  );
}

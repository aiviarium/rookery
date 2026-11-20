// The page a person meets before they are signed in.
import { Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Button, Callout, Card, Icon } from "@aiviarium/nocturne";

import { useShell } from "./shell.tsx";

/** SignIn asks how to sign in, and says what to do about it. */
export function SignIn(): JSX.Element {
  const shell = useShell();
  const config = shell.config;
  const status = () => shell.signIn().status;

  // The edge sends the browser on to the provider, and back to the page the
  // person was on.
  const startOIDC = () => {
    const next = typeof location === "undefined" ? "/" : `${location.pathname}${location.search}`;
    return `/auth/oidc/start?next=${encodeURIComponent(next)}`;
  };

  return (
    <div class="shell-centered">
      <div class="shell-signin nc-stack">
        <div class="nc-row nc-row--tight">
          <Icon name="server" size="lg" />
          <h1 class="nc-type-title" style={{ margin: "0" }}>Rookery</h1>
        </div>
        <p class="nc-muted">Sign in to this server.</p>

        <Show when={status() === "error"}>
          <Callout tone="danger">
            The server could not be asked who you are. Check that it is running, then try again.
          </Callout>
        </Show>

        <Card>
          <div class="nc-stack" style={{ gap: "var(--space-3)" }}>
            <Show
              when={config().identityProvider}
              fallback={
                <>
                  <p class="nc-muted" style={{ margin: "0" }}>
                    This server signs people in with a one-time link. Ask an admin for one.
                  </p>
                  <p class="nc-meta" style={{ margin: "0" }}>
                    On the server's own machine, <code class="nc-code">rookery server setup-key</code> prints a link
                    for the first admin.
                  </p>
                </>
              }
            >
              <Button variant="primary" href={startOIDC()}>
                Sign in with your identity provider
              </Button>
            </Show>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => {
                void shell.signIn.refresh();
              }}
            >
              Already signed in? Check again
            </Button>
          </div>
        </Card>
      </div>
    </div>
  );
}

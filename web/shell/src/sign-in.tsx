// The page a person meets before they are signed in.
import { Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Button, Callout, Card, Icon, InfoTip } from "@aiviarium/nocturne";

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
        <Show when={status() === "error"}>
          <Callout tone="danger">
            The server did not answer
          </Callout>
        </Show>

        <Card>
          <div class="nc-stack" style={{ gap: "var(--space-3)" }}>
            <Show
              when={config().identityProvider}
              fallback={
                <div class="nc-row nc-row--tight">
                  <span class="nc-type-label">Ask an admin for a sign-in link</span>
                  <InfoTip label="Sign-in link">
                    On the server's own machine, <code class="nc-mono">rookery server setup-key</code> prints a link
                    for the first admin.
                  </InfoTip>
                </div>
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
              Check again
            </Button>
          </div>
        </Card>
      </div>
    </div>
  );
}

// The status bar holds only what is out of the ordinary: a link to the server
// that dropped, plugins that could not load. While all is well it is empty.
import { Show, createEffect, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { State } from "@aiviarium/nocturne";

import { useShell } from "../shell.tsx";

/** StatusBar shows a lost connection and failed plugins, and nothing else. */
export function StatusBar(): JSX.Element {
  const shell = useShell();
  const status = () => shell.connection.status();
  // The first connect is the page loading, which the page shows; a drop after it is news.
  const [seenOnline, setSeenOnline] = createSignal(false);
  createEffect(status, (now) => {
    if (now === "online") setSeenOnline(true);
  });
  return (
    <>
      <Show when={seenOnline() && status() !== "online"}>
        <span data-part="connection-state" title="The shell's stream to the server">
          <State status={status() === "connecting" ? "warn" : "error"}>
            {status() === "connecting" ? "Reconnecting…" : "Offline"}
          </State>
        </span>
      </Show>
      <Show when={shell.plugins.error()}>
        {(error) => (
          <span title={error().message}>
            <State status="error">Plugins did not load</State>
          </span>
        )}
      </Show>
    </>
  );
}

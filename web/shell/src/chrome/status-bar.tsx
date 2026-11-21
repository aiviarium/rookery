// The status bar is there only while something is out of the ordinary: a link
// to the server that dropped, plugins that could not load. While all is well
// there is no bar at all; a workbench that wants one draws its own.
import { Show, createEffect, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { State } from "@aiviarium/nocturne";

import { useShell } from "../shell.tsx";

/** useStatusBar is the bar's content while there is news, and undefined otherwise. */
export function useStatusBar(): () => JSX.Element | undefined {
  const shell = useShell();
  const status = () => shell.connection.status();
  // The first connect is the page loading, which the page shows; a drop after it is news.
  const [seenOnline, setSeenOnline] = createSignal(false);
  createEffect(status, (now) => {
    if (now === "online") setSeenOnline(true);
  });
  const dropped = () => seenOnline() && status() !== "online";
  return () =>
    dropped() || shell.plugins.error() !== undefined ? (
      <>
        <Show when={dropped()}>
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
    ) : undefined;
}

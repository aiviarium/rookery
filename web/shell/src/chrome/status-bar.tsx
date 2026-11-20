// The bottom line: whether the shell is talking to the server, and where it is.
import { Show } from "solid-js";
import type { JSX } from "@solidjs/web";

import { useShell } from "../shell.tsx";
import { Glyph } from "../ui.tsx";

/** StatusBar is the connection, the server and the space. */
export function StatusBar(): JSX.Element {
  const shell = useShell();

  const status = shell.connection.status;
  const label = () => {
    switch (status()) {
      case "online":
        return "Connected";
      case "connecting":
        return "Connecting…";
      default:
        return "Offline";
    }
  };
  const dot = () => {
    switch (status()) {
      case "online":
        return "nc-dot nc-dot--live";
      case "connecting":
        return "nc-dot nc-dot--warn";
      default:
        return "nc-dot nc-dot--error";
    }
  };

  return (
    <footer class="nc-shell__status">
      <span class="nc-row" style={{ gap: "var(--space-1)" }} title="The shell's stream to the server">
        <span class={dot()} />
        <span class="nc-meta">{label()}</span>
      </span>
      <span class="nc-row nc-meta" style={{ gap: "var(--space-1)" }}>
        <Glyph name="server" size={12} />
        {shell.rookery.baseUrl}
      </span>
      <Show when={shell.spaces.currentSpace()}>
        {space => (
          <span class="nc-row nc-meta" style={{ gap: "var(--space-1)" }}>
            <Glyph name="space" size={12} />
            {space().name}
          </span>
        )}
      </Show>
      <span class="nc-meta" style={{ "margin-left": "auto" }}>
        {shell.plugins.halves().length} plugin{shell.plugins.halves().length === 1 ? "" : "s"}
      </span>
    </footer>
  );
}

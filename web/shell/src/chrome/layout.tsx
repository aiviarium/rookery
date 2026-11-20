// The shell around every page: the sidebar, the top bar, the page, the status.
import { Show, createEffect, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useNavigate } from "@solidjs/router";

import { activateHalves } from "../plugins.ts";
import { useShell } from "../shell.tsx";
import { Callout } from "../ui.tsx";
import { Sidebar } from "./sidebar.tsx";
import { StatusBar } from "./status-bar.tsx";
import { TopBar } from "./top-bar.tsx";

/** ShellLayout is the chrome the signed-in shell draws around its pages. */
export function ShellLayout(props: { children?: JSX.Element }): JSX.Element {
  const shell = useShell();
  const navigate = useNavigate();
  const started = new Set<string>();

  onSettled(() => {
    shell.setRouterNavigation((path) => navigate(path));
    return () => shell.setRouterNavigation(undefined);
  });

  // A half starts once, when it has loaded: `start` is where it registers what
  // it keeps for the whole page.
  createEffect(
    () => shell.plugins.halves(),
    (halves) => {
      activateHalves(shell.rookery, halves, (path) => navigate(path), started);
    },
  );

  return (
    <div class="nc-shell" data-fill>
      <Sidebar />
      <header class="nc-shell__header">
        <TopBar />
      </header>
      <main class="nc-shell__content" style={{ display: "block" }}>
        <Show when={shell.plugins.error()}>
          {error => <Callout tone="warning">A plugin's client half could not be loaded: {error().message}</Callout>}
        </Show>
        {props.children}
      </main>
      <StatusBar />
    </div>
  );
}

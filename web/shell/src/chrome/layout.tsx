// The shell around every page: the sidebar, the app's header, the frame, the status.
import { Loading, Show, createEffect, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useNavigate } from "@solidjs/router";
import { AppShell, Callout, Frame, LinkProvider, Skeleton } from "@aiviarium/nocturne";

import { activateHalves } from "../plugins.ts";
import { useShell } from "../shell.tsx";
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
    // Nocturne's links in the sidebar, the crumbs and the rows move through the router, not the browser.
    <LinkProvider navigate={path => navigate(path)}>
      <AppShell fill header={<TopBar />} statusbar={<StatusBar />} sidebar={<Sidebar />}>
        <Frame class="shell-frame">
          <Show when={shell.plugins.error()}>
            {error => <Callout tone="warning">A plugin's client half could not be loaded: {error().message}</Callout>}
          </Show>
          <Loading fallback={<Skeleton width="60%" height="14px" />}>{props.children}</Loading>
        </Frame>
      </AppShell>
    </LinkProvider>
  );
}

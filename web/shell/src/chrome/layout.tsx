// The chrome around every page: the shared sidebar, the frame the page sits in,
// the quiet status bar and the toasts. There is no bar across the top on a
// desktop: the frame starts at the top, and each page keeps its path and
// actions in its own header. On a phone the sidebar is a drawer behind the top
// bar's button.
//
// The space the address is in scopes the client's calls. The page is drawn
// afresh when the address moves into another space or out of one, and the
// client is scoped before the page asks anything.
import { For, Loading, createEffect, createSignal, onSettled, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useLocation, useNavigate } from "@solidjs/router";
import { AppShell, Frame, IconButton, LinkProvider, Skeleton, ToastRegion } from "@aiviarium/nocturne";

import { overlays } from "../pages/index.ts";
import { activateHalves } from "../plugins.ts";
import { useShell } from "../shell.tsx";
import { ChromeContext, type Chrome } from "./context.ts";
import type { LevelRef } from "./levels.ts";
import { Sidebar } from "./sidebar.tsx";
import { useStatusBar } from "./status-bar.tsx";

/** WIDTH_KEY and HIDDEN_KEY are where the browser keeps the sidebar's width and whether it is hidden. */
const WIDTH_KEY = "rookery-sidebar-width";
const HIDDEN_KEY = "rookery-sidebar-hidden";

/** ShellLayout is the chrome the signed-in shell draws around its pages. */
export function ShellLayout(props: { children?: JSX.Element }): JSX.Element {
  const shell = useShell();
  const navigate = useNavigate();
  const location = useLocation();
  const started = new Set<string>();
  shell.spaces.follow(() => location.pathname);

  const [width, setWidth] = createSignal(Number(read(WIDTH_KEY)) || 240);
  const [hidden, setHidden] = createSignal(read(HIDDEN_KEY) === "true");
  const [drawer, setDrawer] = createSignal(false);
  const [drilled, setDrilled] = createSignal<LevelRef>();
  // A page of its own puts the sidebar back at the page's level.
  createEffect(
    () => location.pathname,
    () => setDrilled(undefined),
  );

  const chrome: Chrome = {
    sidebarHidden: hidden,
    setSidebarHidden: (next) => {
      setHidden(next);
      write(HIDDEN_KEY, next ? "true" : "");
    },
    closeDrawer: () => setDrawer(false),
    drilled,
    drill: (level) => setDrilled(level),
  };

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

  const statusbar = useStatusBar();

  return (
    // Nocturne's links in the sidebar, the crumbs and the rows move through the router, not the browser.
    <LinkProvider navigate={(path) => navigate(path)}>
      <ChromeContext value={chrome}>
        <AppShell
          fill
          sidebar={<Sidebar />}
          sidebarLabel="Navigation"
          sidebarWidth={width()}
          onSidebarResize={(next) => {
            setWidth(next);
            write(WIDTH_KEY, String(next));
          }}
          sidebarHidden={hidden()}
          drawerOpen={drawer()}
          onDrawerClose={() => setDrawer(false)}
          topbar={
            <>
              <IconButton label="Open the navigation" icon="sidebar" onClick={() => setDrawer(true)} />
              <span class="label">{shell.spaces.currentSpace()?.name ?? "Rookery"}</span>
            </>
          }
          statusbar={statusbar()}
        >
          <Frame flush class="shell-frame">
            <Loading fallback={<Skeleton width="40%" height="14px" />}>
              <For each={[shell.spaces.current()]}>
                {(space) => {
                  shell.spaces.scope(untrack(() => space));
                  return props.children;
                }}
              </For>
            </Loading>
          </Frame>
        </AppShell>
        <For each={[...overlays]}>{(Overlay) => <Overlay />}</For>
        <ToastRegion />
      </ChromeContext>
    </LinkProvider>
  );
}

function read(key: string): string {
  try {
    return localStorage.getItem(key) ?? "";
  } catch {
    return "";
  }
}

function write(key: string, value: string): void {
  try {
    if (value === "") localStorage.removeItem(key);
    else localStorage.setItem(key, value);
  } catch {
    // Without storage the choice lasts for this page.
  }
}

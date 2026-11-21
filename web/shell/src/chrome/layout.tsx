// The chrome around every page: the shared sidebar, the frame the page sits in,
// the quiet status bar and the toasts. There is no bar across the top on a
// desktop: the frame starts at the top, and each page keeps its path and
// actions in its own header. On a phone the sidebar is a drawer behind the top
// bar's button.
import { Loading, createEffect, createSignal, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useNavigate } from "@solidjs/router";
import { AppShell, Frame, IconButton, LinkProvider, Skeleton, ToastRegion } from "@aiviarium/nocturne";

import { activateHalves } from "../plugins.ts";
import { useShell } from "../shell.tsx";
import { ChromeContext, type Chrome } from "./context.ts";
import { Sidebar } from "./sidebar.tsx";
import { StatusBar } from "./status-bar.tsx";

/** WIDTH_KEY and HIDDEN_KEY are where the browser keeps the sidebar's width and whether it is hidden. */
const WIDTH_KEY = "rookery-sidebar-width";
const HIDDEN_KEY = "rookery-sidebar-hidden";

/** ShellLayout is the chrome the signed-in shell draws around its pages. */
export function ShellLayout(props: { children?: JSX.Element }): JSX.Element {
  const shell = useShell();
  const navigate = useNavigate();
  const started = new Set<string>();

  const [width, setWidth] = createSignal(Number(read(WIDTH_KEY)) || 240);
  const [hidden, setHidden] = createSignal(read(HIDDEN_KEY) === "true");
  const [drawer, setDrawer] = createSignal(false);

  const chrome: Chrome = {
    sidebarHidden: hidden,
    setSidebarHidden: (next) => {
      setHidden(next);
      write(HIDDEN_KEY, next ? "true" : "");
    },
    closeDrawer: () => setDrawer(false),
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
          statusbar={<StatusBar />}
        >
          <Frame flush class="shell-frame">
            <Loading fallback={<Skeleton width="40%" height="14px" />}>{props.children}</Loading>
          </Frame>
        </AppShell>
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

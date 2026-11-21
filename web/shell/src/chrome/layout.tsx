// The chrome around every page: the shared sidebar, the workbench of page tabs,
// the quiet status bar and the toasts. There is no bar across the top on a
// desktop: the frames start at the top, and each page keeps its path and
// actions in its own header. On a phone the sidebar is a drawer behind the top
// bar's button.
//
// The browser's address is the focused tab's: a tab that moves moves the
// address, and an address that moves (the sidebar, the back button) moves the
// focused tab. The space the address is in scopes the client's own calls, the
// sidebar's and the plugins'; each tab's page has a client scoped to its own.
import { For, createEffect, createMemo, createSignal, onSettled, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useLocation, useNavigate } from "@solidjs/router";
import { AppShell, IconButton, LinkProvider, ToastRegion } from "@aiviarium/nocturne";

import { overlays } from "../pages/index.ts";
import { activateHalves } from "../plugins.ts";
import { ShellContext, useShell } from "../shell.tsx";
import { ChromeContext, type Chrome } from "./context.ts";
import type { LevelRef } from "./levels.ts";
import { Sidebar } from "./sidebar.tsx";
import { useStatusBar } from "./status-bar.tsx";
import { createWorkbench } from "./workbench.ts";
import { WorkbenchView } from "./workbench-view.tsx";

/** WIDTH_KEY and HIDDEN_KEY are where the browser keeps the sidebar's width and whether it is hidden. */
const WIDTH_KEY = "rookery-sidebar-width";
const HIDDEN_KEY = "rookery-sidebar-hidden";

/** ShellLayout is the chrome the signed-in shell draws around its pages. */
export function ShellLayout(): JSX.Element {
  const shell = useShell();
  const navigate = useNavigate();
  const location = useLocation();
  const started = new Set<string>();
  shell.spaces.follow(() => location.pathname);
  const here = () => `${location.pathname}${location.search}`;
  const workbench = createWorkbench(untrack(here));

  // The addresses the workbench sent the browser to, oldest first. The address
  // router commits a move later, so one of these can arrive after the tab moved
  // on; it is the workbench's own echo, not a move of the address.
  let sent: string[] = [];
  // An address that moves takes the focused tab with it.
  createEffect(here, (address, previous) => {
    // The first run is the address the workbench started from, which it shows.
    if (previous === undefined) return;
    const echo = sent.indexOf(address);
    if (echo >= 0) {
      sent = sent.slice(echo + 1);
      return;
    }
    const id = untrack(workbench.focused);
    if (untrack(() => workbench.tab(id)?.path) !== address) workbench.go(id, address);
  });
  // A tab that moves takes the address with it; a tab that takes the focus
  // replaces the address, so the back button goes back in a tab, not across them.
  const focusedPath = createMemo(() => workbench.tab(workbench.focused())?.path);
  createEffect(
    () => ({ id: workbench.focused(), path: focusedPath() }),
    (next, previous) => {
      if (next.path === undefined || next.path === untrack(here)) return;
      sent = [...sent.slice(-15), next.path];
      navigate(next.path, { replace: previous === undefined || previous.id !== next.id });
    },
  );
  // The sidebar's and the plugins' calls are in the focused tab's space.
  createEffect(
    () => shell.spaces.current(),
    (space) => shell.spaces.scope(space),
  );

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
    shell.setTabOpener((path, place) => workbench.open(path, { edge: place?.edge }));
    return () => {
      shell.setRouterNavigation(undefined);
      shell.setTabOpener(undefined);
    };
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
          sidebar={
            <For each={[shell.spaces.current()]}>
              {(space) => (
                <ShellContext value={{ ...shell, rookery: shell.rookery.inSpace(space) }}>
                  <Sidebar />
                </ShellContext>
              )}
            </For>
          }
          sidebarLabel="Navigation"
          sidebarWidth={width()}
          onSidebarResize={(next) => {
            setWidth(next);
            write(WIDTH_KEY, String(next));
          }}
          sidebarHidden={hidden()}
          onSidebarHiddenChange={chrome.setSidebarHidden}
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
          <WorkbenchView workbench={workbench} />
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

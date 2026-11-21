// The workbench drawn: the Dock of page tabs, and each tab's page under a
// router, a client and a space of its own.
//
// A tab's page reads the shell as every page does, but the shell it reads is
// the tab's: its client is scoped to the tab's space, `shell.navigate` moves
// the tab, and its links move the tab, whichever frame has the focus. On a
// phone there is one page at a time: the focused tab's.
import { For, Loading, Show, createContext, createEffect, createRenderEffect, createSignal, onCleanup, untrack, useContext } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useLocation, useNavigate } from "@solidjs/router";
import {
  Dock,
  LinkProvider,
  type MenuEntry,
  Skeleton,
  ViewChrome,
  dockFrames,
  usePhone,
} from "@aiviarium/nocturne";

import { createTabRouter } from "../router.ts";
import { ShellContext, type Shell, useShell } from "../shell.tsx";
import { spaceOf } from "../spaces.ts";
import { HOME, type Workbench, tabLabel } from "./workbench.ts";

/** PageTab is what a page knows of its tab: whether it is in view, and whether it has the focus. */
export interface PageTab {
  readonly shown: () => boolean;
  readonly focused: () => boolean;
}

const PageTabContext = createContext<PageTab>({ shown: () => true, focused: () => true });

/** usePageTab is the tab of the page that asks. */
export function usePageTab(): PageTab {
  return useContext(PageTabContext);
}

/** WorkbenchView is the frames and tabs that hold the shell's pages. */
export function WorkbenchView(props: { workbench: Workbench }): JSX.Element {
  const workbench = props.workbench;
  const phone = usePhone();
  const menu = (): MenuEntry[] => [
    { id: "duplicate", label: "Duplicate" },
    { id: "right", label: "Open to the right", icon: "split-right" },
    { id: "bottom", label: "Open below", icon: "split-down" },
    { type: "separator" },
    { id: "others", label: "Close the others" },
    { id: "close", label: "Close" },
  ];
  const pathOf = (id: string) => workbench.tab(id)?.path ?? HOME;

  return (
    <Show
      when={!phone()}
      fallback={
        <For each={[workbench.focused()]}>{(id) => <TabPage workbench={workbench} id={id} shown={() => true} />}</For>
      }
    >
      <Dock
        class="shell-workbench"
        label="Pages"
        layout={workbench.layout()}
        onLayoutChange={(layout) => workbench.setLayout(layout)}
        tab={(id) => ({ label: tabLabel(workbench.tab(id)) })}
        onClose={(id) => workbench.close(id)}
        closeLabel={(item) => `Close ${item.label}`}
        onNewTab={(frame) => workbench.open(HOME, { frame })}
        onSplit={(frame, edge) => {
          const shown = dockFrames(workbench.layout()).find((one) => one.id === frame)?.active ?? "";
          workbench.open(shown === "" ? HOME : pathOf(shown), { frame, edge });
        }}
        menu={menu}
        onMenuSelect={(id, entry) => {
          const frame = workbench.frameOf(id);
          if (entry === "duplicate") workbench.open(pathOf(id), { frame });
          else if (entry === "right" || entry === "bottom") workbench.open(pathOf(id), { frame, edge: entry });
          else if (entry === "others") workbench.closeOthers(id);
          else if (entry === "close") workbench.close(id);
        }}
      >
        {(id, shown) => <LazyTab workbench={workbench} id={id} shown={shown} />}
      </Dock>
    </Show>
  );
}

/** LazyTab draws a tab's page the first time it shows, and keeps it after. */
function LazyTab(props: { workbench: Workbench; id: string; shown: () => boolean }): JSX.Element {
  const [drawn, setDrawn] = createSignal(false);
  createEffect(props.shown, (shown) => {
    if (shown) setDrawn(true);
  });
  return (
    <Show when={drawn() || props.shown()}>
      <TabPage workbench={props.workbench} id={props.id} shown={props.shown} />
    </Show>
  );
}

/** TabPage is one tab's router and page. */
function TabPage(props: { workbench: Workbench; id: string; shown: () => boolean }): JSX.Element {
  const Router = createTabRouter(untrack(() => props.workbench.tab(props.id)?.path ?? HOME));
  return (
    <Router>
      {(content) => (
        <TabBody workbench={props.workbench} id={props.id} shown={props.shown}>
          {content.children}
        </TabBody>
      )}
    </Router>
  );
}

/** TabBody gives a tab's page its shell: the tab's client, space, links and name. */
function TabBody(props: {
  workbench: Workbench;
  id: string;
  shown: () => boolean;
  children?: JSX.Element;
}): JSX.Element {
  const shell = useShell();
  const navigate = useNavigate();
  const location = useLocation();
  onCleanup(props.workbench.register(props.id, (path, options) => navigate(path, options)));
  // The workbench keeps where the tab is. A move is a transition that ends when
  // the page settles, which a live page never does, and what is written inside
  // it stays there; so the path is recorded outside it, as soon as the page
  // reads it.
  createRenderEffect(
    () => `${location.pathname}${location.search}`,
    (path) => {
      queueMicrotask(() => props.workbench.setPath(props.id, path));
    },
  );

  const spaces = shell.spaces.at(() => location.pathname);
  const scoped = (space: string): Shell => ({
    ...shell,
    rookery: shell.rookery.inSpace(space),
    spaces,
    navigate: (path) => navigate(path),
    setRouterNavigation: () => undefined,
    openTab: (path, place) => {
      const frame = props.workbench.frameOf(props.id);
      props.workbench.open(path, { frame, edge: place?.edge });
    },
  });
  const tab: PageTab = {
    shown: () => props.shown(),
    focused: () => props.workbench.focused() === props.id,
  };
  const chrome = {
    pinnable: () => false,
    pinned: () => false,
    togglePin: () => undefined,
    // Outside the move's transition too, as the path is.
    setTitle: (title: string | undefined) => {
      queueMicrotask(() => props.workbench.setTitle(props.id, title));
    },
    // The sidebar's head shows where the person is; a page's header holds its actions.
    path: () => false,
  };

  // Every router hears a click on any link of the document, the browser
  // address's first; a link in this tab moves this tab.
  const onClick = (event: MouseEvent) => {
    const to = ownLink(event);
    if (to === undefined) return;
    event.preventDefault();
    navigate(to);
  };

  return (
    <LinkProvider navigate={(path) => navigate(path)}>
      <ViewChrome value={chrome}>
        <PageTabContext value={tab}>
        <div class="shell-tab" data-page-tab={props.id} onClick={onClick}>
          <Loading fallback={<Skeleton width="40%" height="14px" />}>
            <For each={[spaceOf(location.pathname)]}>
              {(space) => <ShellContext value={scoped(space)}>{props.children}</ShellContext>}
            </For>
          </Loading>
        </div>
        </PageTabContext>
      </ViewChrome>
    </LinkProvider>
  );
}

/** ownLink is the path a plain click on a link of this origin goes to. */
function ownLink(event: MouseEvent): string | undefined {
  if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.altKey || event.ctrlKey || event.shiftKey) {
    return undefined;
  }
  const anchor = event.composedPath().find((node): node is HTMLAnchorElement => node instanceof HTMLAnchorElement);
  if (anchor === undefined || anchor.target !== "" || anchor.hasAttribute("download")) return undefined;
  const href = anchor.getAttribute("href");
  if (href === null || href === "") return undefined;
  if ((anchor.getAttribute("rel") ?? "").split(/\s+/).includes("external")) return undefined;
  const url = new URL(anchor.href, document.baseURI);
  if (url.origin !== location.origin || (url.protocol !== "http:" && url.protocol !== "https:")) return undefined;
  return url.pathname + url.search + url.hash;
}

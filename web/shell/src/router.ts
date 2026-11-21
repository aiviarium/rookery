// The shell's routers.
//
// Every tab of the workbench has a router of its own over the shell's pages,
// then the plugins' pages, then not found. The plugins' pages are not known
// when this module is built, so one catch-all route resolves them against what
// the installed halves declared.
//
// The browser's address has a router too, which draws no page: it holds the
// path of the focused tab, so the sidebar, a reload and the back button follow
// that tab.
import { createRouter, memoryHistory } from "@solidjs/router";

import { routes } from "./pages/index.ts";
import { PluginRoute } from "./plugin/route.tsx";

/** pageRoutes are the routes a tab draws. */
export const pageRoutes = [...routes, { path: "*404", component: PluginRoute }];

/** Router is the browser address's router. */
export const Router = createRouter({
  routes: [{ path: "*all", component: () => null }],
});

/** createTabRouter is a tab's router, starting at a path. */
export function createTabRouter(path: string): ReturnType<typeof createRouter> {
  return createRouter({ routes: pageRoutes, history: memoryHistory(path) });
}

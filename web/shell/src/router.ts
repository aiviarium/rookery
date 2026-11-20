// The shell's router: its own pages, then the plugins' pages, then not found.
//
// The plugins' pages are not known when this module is built, so one catch-all
// route resolves them against what the installed halves declared.
import { createRouter } from "@solidjs/router";

import { routes } from "./pages/index.ts";
import { PluginRoute } from "./plugin/route.tsx";

export const Router = createRouter({
  routes: [...routes, { path: "*404", component: PluginRoute }],
});

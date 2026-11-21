// Home and the inbox. Both are at the top of the sidebar, which draws them
// itself; the area adds their pages.
import { defineRoutes } from "@solidjs/router";

import { HomePage } from "./home.tsx";
import { InboxPage } from "./inbox.tsx";

export const routes = defineRoutes([
  { path: "/", component: HomePage },
  { path: "/inbox", component: InboxPage },
]);

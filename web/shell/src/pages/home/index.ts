// Home: what the server holds and where to go next, and the person's inbox.
import { defineRoutes } from "@solidjs/router";

import type { Area } from "../types.ts";
import { HomePage } from "./home.tsx";
import { InboxPage } from "./inbox.tsx";

export const routes = defineRoutes([
  { path: "/", component: HomePage },
  { path: "/inbox", component: InboxPage },
]);

export const nav: Area["nav"] = [
  { label: "Home", href: "/", icon: "home" },
  { label: "Inbox", href: "/inbox", icon: "inbox" },
];

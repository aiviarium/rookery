// Home: what the server holds and where to go next.
import { defineRoutes } from "@solidjs/router";

import type { Area } from "../types.ts";
import { HomePage } from "./home.tsx";

export const routes = defineRoutes([{ path: "/", component: HomePage }]);

export const nav: Area["nav"] = [{ label: "Home", href: "/", icon: "home" }];

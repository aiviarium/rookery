// The seam between the shell and its areas.
//
// Every area is a folder here whose `index.ts` exports `routes` and `nav`. This
// file lists the areas and joins what they export, so an area adds its pages and
// navigation without touching another area's files.
//
// An area with no pages of its own yet stays out of the list.
import { defineRoutes, type RouteDefinition } from "@solidjs/router";

import * as admin from "./admin/index.ts";
import * as home from "./home/index.ts";
import * as infra from "./infra/index.tsx";
import * as settings from "./settings/index.tsx";
import type { Area, Domain, NavEntry } from "./types.ts";

export type { Area, Domain, NavEntry } from "./types.ts";

/** AREAS is every area the shell has. Add one folder, then one entry here. */
const AREAS: readonly Area[] = [home, infra, admin, settings];

/** routes is every path the shell's own pages answer. */
export const routes: readonly RouteDefinition[] = defineRoutes(AREAS.flatMap((area) => area.routes));

/** nav is every entry the areas add to the navigation, in area order. */
export const nav: readonly NavEntry[] = AREAS.flatMap((area) => area.nav);

/** domains are the places the sidebar drills down into, in area order. */
export const domains: readonly Domain[] = AREAS.flatMap((area) => area.domains ?? []);

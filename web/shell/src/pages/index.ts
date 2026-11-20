// The seam between the shell and its areas.
//
// Every area is a folder here whose `index.ts` exports `routes` and `nav`. This
// file lists the areas and joins what they export, so an area adds its pages and
// navigation without touching another area's files.
//
// An area with no pages of its own yet stays out of the list.
import { defineRoutes, type RouteDefinition } from "@solidjs/router";

import * as home from "./home/index.ts";
import * as infra from "./infra/index.ts";
import type { Area, NavEntry } from "./types.ts";

export type { Area, NavEntry } from "./types.ts";

/** AREAS is every area the shell has. Add one folder, then one entry here. */
const AREAS: readonly Area[] = [home, infra];

/** routes is every path the shell's own pages answer. */
export const routes: readonly RouteDefinition[] = defineRoutes(AREAS.flatMap((area) => area.routes));

/** nav is every entry the areas add to the navigation, in area order. */
export const nav: readonly NavEntry[] = AREAS.flatMap((area) => area.nav);

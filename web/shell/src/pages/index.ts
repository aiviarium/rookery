// The seam between the shell and its areas.
//
// Every area is a folder here whose `index.ts` exports `routes`, and its
// `levels` and `top` sections when it has them. This file lists the areas and
// joins what they export, so an area adds its pages and navigation without
// touching another area's files.
import { defineRoutes, type RouteDefinition } from "@solidjs/router";
import type { JSX } from "@solidjs/web";

import type { Level } from "../chrome/levels.ts";
import * as admin from "./admin/index.tsx";
import * as fleet from "./fleet/index.tsx";
import * as home from "./home/index.ts";
import * as settings from "./settings/index.tsx";
import * as spaces from "./spaces/index.tsx";
import type { Area, TopSection } from "./types.ts";

export type { Area, TopSection } from "./types.ts";

/** AREAS is every area the shell has. Add one folder, then one entry here. */
const AREAS: readonly Area[] = [home, fleet, spaces, admin, settings];

/** routes is every path the shell's own pages answer. */
export const routes: readonly RouteDefinition[] = defineRoutes(AREAS.flatMap((area) => area.routes));

/** levels are the containers the sidebar drills into. */
export const levels: readonly Level[] = AREAS.flatMap((area) => area.levels ?? []);

/** overlays are drawn over every page. */
export const overlays: ReadonlyArray<() => JSX.Element> = AREAS.flatMap((area) => area.overlays ?? []);

/** topSections are what the areas add to the top of the sidebar. */
export const topSections: readonly TopSection[] = AREAS.flatMap((area) => area.top ?? []);

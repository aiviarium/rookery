// What one area of the shell contributes: the routes it answers, the levels of
// the sidebar it adds, and its sections at the top of the sidebar.
//
// An area is a folder under pages/ with an `index.ts` that exports these. The
// seam in pages/index.ts lists the areas, so a new area never changes another
// area's files, and the chrome never names an area.
import type { RouteDefinition } from "@solidjs/router";
import type { JSX } from "@solidjs/web";

import type { Level } from "../chrome/levels.ts";
import type { SidebarGroup } from "../chrome/sidebar.tsx";

/** TopSection is what an area adds to the top of the sidebar. */
export interface TopSection {
  /** order places the section among the others: lower comes first. */
  readonly order: number;
  /**
   * use reads what the section needs, in the sidebar's scope, and returns its
   * groups. It runs once each time the top level is drawn.
   */
  readonly use: () => () => SidebarGroup[];
}

/** Area is one part of the shell's own pages. */
export interface Area {
  /** routes are the paths the area answers. */
  readonly routes: readonly RouteDefinition[];
  /** levels are the containers in it the sidebar drills into. */
  readonly levels?: readonly Level[];
  /** top is what it adds to the top of the sidebar. */
  readonly top?: readonly TopSection[];
  /** overlays are drawn over every page, such as a dialog the sidebar opens. */
  readonly overlays?: ReadonlyArray<() => JSX.Element>;
}

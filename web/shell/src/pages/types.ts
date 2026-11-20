// What one area of the shell contributes: the routes it answers, and where it
// sits in the navigation.
//
// An area is a folder under pages/ with an `index.ts` that exports `routes` and
// `nav`. The seam in pages/index.ts lists the areas, so a new area never changes
// another area's files.
import type { RouteDefinition } from "@solidjs/router";
import type { IconName } from "@aiviarium/nocturne";

/** NavEntry is one entry an area adds to the shell's navigation. */
export interface NavEntry {
  /** label is what the entry says. */
  readonly label: string;
  /** href is the path it opens. */
  readonly href: string;
  /** group is the section it sits in; empty for the first section. */
  readonly group?: string;
  /** icon is Nocturne's glyph before the label. */
  readonly icon?: IconName;
}

/** Area is one part of the shell's own pages. */
export interface Area {
  /** routes are the paths the area answers. */
  readonly routes: readonly RouteDefinition[];
  /** nav is where the area sits in the navigation. */
  readonly nav: readonly NavEntry[];
}

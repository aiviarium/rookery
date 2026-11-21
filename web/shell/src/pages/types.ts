// What one area of the shell contributes: the routes it answers, where it sits
// in the navigation, and the domains the sidebar drills down into.
//
// An area is a folder under pages/ with an `index.ts` that exports `routes`,
// `nav` and, when it has one, `domains`. The seam in pages/index.ts lists the
// areas, so a new area never changes another area's files.
import type { RouteDefinition } from "@solidjs/router";
import type { JSX } from "@solidjs/web";
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
  /** admin keeps the entry to admins, who alone can use its page. */
  readonly admin?: boolean;
}

/**
 * Domain is a place the sidebar drills down into: inside it, the sidebar shows
 * the domain's own places, with a way back, as Nocturne's navigation does for a
 * project or a host.
 */
export interface Domain {
  /** pattern is the path inside the domain, such as "/hosts/:name/*". */
  readonly pattern: string;
  /** Sidebar draws the sidebar's header and places while the path is inside. */
  readonly Sidebar: (props: { params: Record<string, string> }) => JSX.Element;
}

/** Area is one part of the shell's own pages. */
export interface Area {
  /** routes are the paths the area answers. */
  readonly routes: readonly RouteDefinition[];
  /** nav is where the area sits in the navigation. */
  readonly nav: readonly NavEntry[];
  /** domains are the places in it the sidebar drills into. */
  readonly domains?: readonly Domain[];
}

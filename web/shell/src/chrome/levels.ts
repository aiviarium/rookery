// The sidebar's levels.
//
// The sidebar is a stack of levels: the top, then containers a person drills
// into, such as the fleet, a host, a space, Administration or Settings. A level
// says which addresses are inside it and draws its header and places. The
// sidebar shows the deepest level that holds the page; a drill row or back
// moves the sidebar alone, and the page stays as it was, until the next page.
import type { JSX } from "@solidjs/web";

/** LevelRef names a level and the values that fill it, such as a host's name. */
export interface LevelRef {
  readonly id: string;
  readonly params?: Readonly<Record<string, string>>;
}

/** LevelProps are what a level's sidebar is drawn with. */
export interface LevelProps {
  /** params fill the level, such as `{ name: "forge" }` for a host. */
  readonly params: Readonly<Record<string, string>>;
  /** inside is true while the page is one of the level's own. */
  readonly inside: boolean;
}

/** Level is one level of the sidebar. */
export interface Level {
  /** id names the level, such as "host". */
  readonly id: string;
  /**
   * match says whether an address is inside the level, and with which values.
   * Deeper levels are asked first, so a host's places are not the fleet's.
   */
  readonly match: (path: string) => Record<string, string> | undefined;
  /** depth orders the levels: the top is 0, a container in it 1, one inside that 2. */
  readonly depth: number;
  /** Sidebar draws the level's header and places. */
  readonly Sidebar: (props: LevelProps) => JSX.Element;
}

/** prefixMatch is a level's `match` for the addresses under any of these paths. */
export function prefixMatch(...prefixes: string[]): Level["match"] {
  return (path) => (prefixes.some((prefix) => path === prefix || path.startsWith(`${prefix}/`)) ? {} : undefined);
}

/** resolveLevel is the deepest level that holds an address, or the top. */
export function resolveLevel(levels: readonly Level[], path: string): LevelRef {
  let found: { level: Level; params: Record<string, string> } | undefined;
  for (const level of levels) {
    const params = level.match(path);
    if (params === undefined) continue;
    if (found === undefined || level.depth > found.level.depth) found = { level, params };
  }
  return found === undefined ? { id: TOP } : { id: found.level.id, params: found.params };
}

/** TOP is the level the sidebar starts at. */
export const TOP = "top";

/** sameLevel says whether two references name the same level with the same values. */
export function sameLevel(a: LevelRef, b: LevelRef): boolean {
  if (a.id !== b.id) return false;
  const left = a.params ?? {};
  const right = b.params ?? {};
  const keys = new Set([...Object.keys(left), ...Object.keys(right)]);
  for (const key of keys) if (left[key] !== right[key]) return false;
  return true;
}

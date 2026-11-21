// The spaces this person may use, and which one the shell is in.
//
// A space is a container the sidebar drills into: the shell is in one while the
// address is under /spaces/{id}, and in none elsewhere. The chrome scopes the
// client's calls to it (`Rookery-Space`) before a page in it asks anything.
import { type Accessor, createSignal } from "solid-js";
import type { Rookery } from "@rookery/client";
import type { Space, SpaceKind } from "@rookery/proto";

/** SPACE_PATH matches an address inside a space, with the space's id. */
const SPACE_PATH = /^\/spaces\/([^/]+)(?:\/|$)/;

/** Spaces is the space list, the kinds there are, and the one the shell is in. */
export interface Spaces {
  /** list is the spaces the person may open, archived ones aside. */
  readonly list: Accessor<readonly Space[]>;
  /** kinds are the kinds of space the plugins declare. */
  readonly kinds: Accessor<readonly SpaceKind[]>;
  /** error is why the list could not be read. */
  readonly error: Accessor<Error | undefined>;
  /** current is the id of the space the address is in, or "" for none. */
  readonly current: Accessor<string>;
  /** currentSpace is the space itself, once the list has it. */
  readonly currentSpace: Accessor<Space | undefined>;
  /** follow hands over the router's path, which `current` reads. The chrome calls it once. */
  follow(path: Accessor<string>): void;
  /** scope makes every following call one in this space, or in none for "". */
  scope(id: string): void;
  /** refresh reads the list again, after a space was made, renamed or archived. */
  refresh(): Promise<void>;
}

/** spacePath is the address of one of a space's places. */
export function spacePath(id: string, place = ""): string {
  const base = `/spaces/${encodeURIComponent(id)}`;
  return place === "" ? base : `${base}/${place}`;
}

/** spaceOf is the id of the space an address is in, or "". */
export function spaceOf(path: string): string {
  const found = SPACE_PATH.exec(path);
  return found === null ? "" : decodeURIComponent(found[1] ?? "");
}

/** createSpaces reads the space list and follows the space in the address. */
export function createSpaces(rookery: Rookery): Spaces {
  const [list, setList] = createSignal<readonly Space[]>([]);
  const [kinds, setKinds] = createSignal<readonly SpaceKind[]>([]);
  const [error, setError] = createSignal<Error>();
  // The path is the router's, which exists only inside it; until the chrome
  // hands it over the shell is in no space.
  let path: Accessor<string> = () => "";
  const current = () => spaceOf(path());

  const refresh = async () => {
    try {
      const [spaces, declared] = await Promise.all([
        rookery.spaces.list({ limit: 500 }),
        rookery.spaces.listKinds({}).catch(() => ({ kinds: [] as SpaceKind[] })),
      ]);
      setList(spaces.spaces.filter((space) => !space.archived));
      setKinds(declared.kinds);
      setError(undefined);
    } catch (failure) {
      setError(failure instanceof Error ? failure : new Error(String(failure)));
    }
  };

  return {
    list,
    kinds,
    error,
    current,
    currentSpace: () => list().find((space) => space.id === current()),
    follow: (next) => {
      path = next;
    },
    scope: (id) => rookery.setSpace(id),
    refresh,
  };
}

/**
 * inSpace puts what holds a page before its crumbs: the space the address is
 * in, or Administration, which holds the same pages for every space at once.
 */
export function inSpace(spaces: Spaces, crumbs: Array<{ label: string; href?: string; onClick?: () => void }>) {
  const id = spaces.current();
  if (id === "") return [{ label: "Administration", href: "/admin" }, ...crumbs];
  return [{ label: spaces.currentSpace()?.name ?? "Space", href: spacePath(id) }, ...crumbs];
}

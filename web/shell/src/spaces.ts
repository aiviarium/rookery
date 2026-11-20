// The spaces this person may use, and which one the shell is in.
//
// Every call carries the current space in `Rookery-Space`; pages that show a
// space's records read `version()` and ask again when it changes.
import { type Accessor, createSignal } from "solid-js";
import type { Rookery } from "@rookery/client";
import type { Space } from "@rookery/proto";

/** SPACE_KEY is where the browser remembers the space it was in. */
const SPACE_KEY = "rookery-space";

/** Spaces is the space list, and the one the shell is in. */
export interface Spaces {
  /** list is what the person may open. */
  readonly list: Accessor<readonly Space[]>;
  /** error is why the list could not be read. */
  readonly error: Accessor<Error | undefined>;
  /** current is the id of the space the shell is in, or "" for none. */
  readonly current: Accessor<string>;
  /** currentSpace is the space itself, when it is in the list. */
  readonly currentSpace: Accessor<Space | undefined>;
  /** version rises on every change, so a page knows to ask again. */
  readonly version: Accessor<number>;
  /** select moves the shell into a space, or out of every space with "". */
  select(id: string): void;
  /** refresh reads the list again, after a space was made or archived. */
  refresh(): Promise<void>;
}

/** createSpaces reads the space list and keeps the current space. */
export function createSpaces(rookery: Rookery): Spaces {
  const stored = readStored();
  if (stored !== "") rookery.setSpace(stored);

  const [list, setList] = createSignal<readonly Space[]>([]);
  const [error, setError] = createSignal<Error>();
  const [current, setCurrent] = createSignal(stored);
  const [version, setVersion] = createSignal(0);

  const select = (id: string) => {
    rookery.setSpace(id);
    setCurrent(id);
    setVersion((count) => count + 1);
    writeStored(id);
  };

  const refresh = async () => {
    try {
      const answer = await rookery.spaces.list({ limit: 200 });
      setList(answer.spaces);
      setError(undefined);
      // A space that was archived or removed while it was open is no longer a
      // place the shell can be.
      if (current() !== "" && !answer.spaces.some((space) => space.id === current())) select("");
    } catch (failure) {
      setError(failure instanceof Error ? failure : new Error(String(failure)));
    }
  };

  return {
    list,
    error,
    current,
    currentSpace: () => list().find((space) => space.id === current()),
    version,
    select,
    refresh,
  };
}

/** readStored is the space this browser was in, when it still asks for one. */
function readStored(): string {
  if (typeof localStorage === "undefined") return "";
  return localStorage.getItem(SPACE_KEY) ?? "";
}

/** writeStored remembers the space, or forgets it for "". */
function writeStored(id: string): void {
  if (typeof localStorage === "undefined") return;
  if (id === "") {
    localStorage.removeItem(SPACE_KEY);
    return;
  }
  localStorage.setItem(SPACE_KEY, id);
}

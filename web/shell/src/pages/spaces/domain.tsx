// The space domain: every place under /spaces/{id} reads the same space, the
// caller's level on it and its kind through this context. The chrome has
// scoped the client's calls to the space before any of them asks.
import { For, Show, createContext, untrack, useContext } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useParams } from "@solidjs/router";
import { Level, type Space } from "@rookery/proto";
import { Button, EmptyState } from "@aiviarium/nocturne";

import { createLive } from "../../parts/data.ts";
import { View } from "../../parts/view.tsx";
import { useShell } from "../../shell.tsx";

/** SpaceDomainValue is what a space's places share. */
export interface SpaceDomainValue {
  /** id is the space's id from the address. It is fixed: another space mounts its places afresh. */
  readonly id: string;
  /** space is the space once it has been read. */
  readonly space: () => Space | undefined;
  /** kindLabel is what the space's kind is called. */
  readonly kindLabel: () => string;
  /** manages is true when the caller may rename, archive and share the space. */
  readonly manages: () => boolean;
  /** refresh reads the space again. */
  readonly refresh: () => void;
}

const SpaceContext = createContext<SpaceDomainValue>();

/** useSpace is the space whose places the page is one of. */
export function useSpace(): SpaceDomainValue {
  const value = useContext(SpaceContext);
  if (value === undefined) throw new Error("useSpace is for the pages under /spaces/:id");
  return value;
}

/** SpaceDomain holds the space the address names, afresh for each space. */
export function SpaceDomain(props: { children?: JSX.Element }): JSX.Element {
  const params = useParams<{ id: string }>();
  return <For each={[params.id ?? ""]}>{(id) => <SpaceScope id={id}>{props.children}</SpaceScope>}</For>;
}

function SpaceScope(props: { id: string; children?: JSX.Element }): JSX.Element {
  const shell = useShell();
  const id = untrack(() => props.id);
  const query = createLive(() => shell.rookery.spaces.get({ space: id }), { events: ["rookery/spaces."] });
  const level = createLive(() => shell.rookery.access.level({ resource: `space://${id}` }).catch(() => undefined));
  const space = () => query()?.space;
  const value: SpaceDomainValue = {
    id,
    space,
    kindLabel: () => {
      const kind = space()?.kind ?? "";
      return shell.spaces.kinds().find((one) => one.id === kind)?.label ?? kind;
    },
    manages: () => shell.signIn.user()?.admin === true || (level()?.level ?? Level.UNSPECIFIED) >= Level.MANAGE,
    refresh: () => {
      void query.refresh().catch(() => undefined);
      void shell.spaces.refresh();
    },
  };
  const missing = () => query() === undefined && query.error() !== undefined;
  return (
    <SpaceContext value={value}>
      <Show when={!missing()} fallback={<Missing />}>
        {props.children}
      </Show>
    </SpaceContext>
  );
}

function Missing(): JSX.Element {
  const shell = useShell();
  return (
    <View id="space-missing" crumbs={[{ label: "Space" }]}>
      <EmptyState icon="layers" title="No such space">
        <Button onClick={() => shell.navigate("/")}>Home</Button>
      </EmptyState>
    </View>
  );
}

/** spaceCrumbs are the path to one of a space's places. */
export function spaceCrumbs(domain: SpaceDomainValue, place?: string, href?: string): Array<{ label: string; href?: string }> {
  const name = domain.space()?.name ?? "Space";
  if (place === undefined) return [{ label: name }];
  return [{ label: name, href: href ?? `/spaces/${encodeURIComponent(domain.id)}` }, { label: place }];
}

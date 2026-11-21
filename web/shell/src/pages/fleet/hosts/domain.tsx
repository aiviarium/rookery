// The host domain: every place under /hosts/{name} reads the same host through
// this context, so the overview, the terminal and the files never ask for it
// three times, and a change to it shows in all of them at once.
import { For, Show, createContext, untrack, useContext } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useLocation, useParams } from "@solidjs/router";
import type { Query } from "@rookery/client/solid";
import { HostType, type Host, type HostServiceGetResponse } from "@rookery/proto";
import { EmptyState, Button, type ViewCrumb } from "@aiviarium/nocturne";

import { createLive } from "../../../parts/data.ts";
import { View } from "../../../parts/view.tsx";
import { useShell } from "../../../shell.tsx";
import { HOST_EVENTS, hostPath } from "./paths.ts";

/** HostDomain is what the host's places share. */
export interface HostDomainValue {
  /** name is the host's name from the path. It is fixed: another host mounts its places afresh. */
  readonly name: () => string;
  /** host is the host once it has been read. */
  readonly host: () => Host | undefined;
  /** workstation is true for a host with a desktop. */
  readonly workstation: () => boolean;
  /** query is the read itself, for its error and to ask again. */
  readonly query: Query<HostServiceGetResponse>;
  /** refresh asks for the host again. */
  readonly refresh: () => void;
  /**
   * crumbs are the path to one of the host's places, deepest last; the host's
   * crumb switches to another host at the same place.
   */
  readonly crumbs: (...places: Array<string | ViewCrumb>) => ViewCrumb[];
}

const HostContext = createContext<HostDomainValue>();

/** useHost is the host whose places the page is one of. */
export function useHost(): HostDomainValue {
  const value = useContext(HostContext);
  if (value === undefined) throw new Error("useHost is for the pages under /hosts/:name");
  return value;
}

/**
 * HostDomain holds the place the router chose for the host in the path. Moving
 * to another host mounts its places afresh, so each place reads a fixed name and
 * its connections and drafts are the host's own.
 */
export function HostDomain(props: { children?: JSX.Element }): JSX.Element {
  const params = useParams<{ name: string }>();
  return <For each={[params.name ?? ""]}>{(name) => <HostScope name={name}>{props.children}</HostScope>}</For>;
}

function HostScope(props: { name: string; children?: JSX.Element }): JSX.Element {
  const shell = useShell();
  const fixed = untrack(() => props.name);
  const name = () => fixed;
  const query = createLive(() => shell.rookery.hosts.get({ name: fixed }), { events: HOST_EVENTS });
  const host = () => query()?.host;
  const location = useLocation();
  const hosts = createLive(() => shell.rookery.hosts.list({ limit: 500 }), { events: HOST_EVENTS });
  // The place the page is at under the host, kept when the crumb switches to another host.
  const place = () => location.pathname.slice(hostPath(fixed).length).replace(/^\//, "");
  const switcher = (): ViewCrumb["switcher"] => ({
    label: "Switch host",
    items: (hosts()?.hosts ?? [{ name: fixed }]).map((one) => ({ id: one.name, label: one.name, checked: one.name === fixed })),
    onSelect: (other) => {
      if (other !== fixed) shell.navigate(hostPath(other, place().startsWith("tab/") ? "" : place()));
    },
  });
  const value: HostDomainValue = {
    name,
    host,
    workstation: () => host()?.type === HostType.WORKSTATION,
    query,
    refresh: () => void query.refresh().catch(() => undefined),
    crumbs: (...places) => hostCrumbs(fixed, places, switcher()),
  };
  // A host that is not there, or not the person's to see, has no places.
  const missing = () => query() === undefined && query.error() !== undefined;
  return (
    <HostContext value={value}>
      <Show when={!missing()} fallback={<Missing name={fixed} message={query.error()?.message ?? ""} />}>
        {props.children}
      </Show>
    </HostContext>
  );
}

function Missing(props: { name: string; message: string }): JSX.Element {
  const shell = useShell();
  return (
    <View id="host-missing" crumbs={hostCrumbs(props.name)}>
      <EmptyState icon="server" title={`No host ${props.name}`} description={props.message || undefined}>
        <Button onClick={() => shell.navigate("/hosts")}>All hosts</Button>
      </EmptyState>
    </View>
  );
}

/** hostCrumbs are the path to one of a host's places, with the switcher on the host's crumb when there is one. */
export function hostCrumbs(name: string, places: ReadonlyArray<string | ViewCrumb> = [], switcher?: ViewCrumb["switcher"]): ViewCrumb[] {
  return [
    { label: "Hosts", href: "/hosts" },
    { label: name, href: places.length === 0 ? undefined : hostPath(name), switcher },
    ...places.map((place) => (typeof place === "string" ? { label: place } : place)),
  ];
}

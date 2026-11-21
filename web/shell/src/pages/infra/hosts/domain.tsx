// The host domain: every place under /hosts/{name} reads the same host through
// this context, so the overview, the terminal and the files never ask for it
// three times, and a change to it shows in all of them at once.
import { For, Show, createContext, untrack, useContext } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useParams } from "@solidjs/router";
import type { Query } from "@rookery/client/solid";
import { HostType, type Host, type HostServiceGetResponse } from "@rookery/proto";
import { EmptyState, Button } from "@aiviarium/nocturne";

import { createLive } from "../../../parts/data.ts";
import { View } from "../../../parts/view.tsx";
import { useShell } from "../../../shell.tsx";
import { HOST_EVENTS } from "./paths.ts";

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
  const value: HostDomainValue = {
    name,
    host,
    workstation: () => host()?.type === HostType.WORKSTATION,
    query,
    refresh: () => void query.refresh().catch(() => undefined),
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
    <View id="host-missing" crumbs={[{ label: "Hosts", href: "/hosts" }, { label: props.name }]}>
      <EmptyState icon="server" title={`No host ${props.name}`} description={props.message}>
        <Button onClick={() => shell.navigate("/hosts")}>Back to the hosts</Button>
      </EmptyState>
    </View>
  );
}

/** hostCrumbs are the path to one of a host's places. */
export function hostCrumbs(name: string, place?: string): Array<{ label: string; href?: string }> {
  const crumbs: Array<{ label: string; href?: string }> = [{ label: "Hosts", href: "/hosts" }];
  if (place === undefined) return [...crumbs, { label: name }];
  return [...crumbs, { label: name, href: `/hosts/${encodeURIComponent(name)}` }, { label: place }];
}

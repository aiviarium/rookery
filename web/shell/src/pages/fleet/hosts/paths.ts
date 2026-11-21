// Where a host's places are. Every page and the sidebar build links with these.

/** hostPath is a host's overview, or one of its places. */
export function hostPath(name: string, place = ""): string {
  const base = `/hosts/${encodeURIComponent(name)}`;
  return place === "" ? base : `${base}/${place}`;
}

/** hostResource is the resource URI jobs and grants name a host by. */
export function hostResource(name: string): string {
  return `host://${name}`;
}

/** HOST_EVENTS are the events that change a host's record. */
export const HOST_EVENTS = ["rookery/hosts."] as const;

/** terminalPath is one of a host's terminals, shown alone in its Terminal place. */
export function terminalPath(host: string, id: string): string {
  return `${hostPath(host, "terminal")}?attach=${encodeURIComponent(id)}`;
}

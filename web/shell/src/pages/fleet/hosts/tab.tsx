// A place a plugin adds to hosts of the types it serves. The plugin's client
// half draws into the view's content; the shell keeps the frame and the path.
import { For, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useParams } from "@solidjs/router";
import { EmptyState } from "@aiviarium/nocturne";

import { HostTabMount } from "../../../plugin/mount.tsx";
import { View } from "../../../parts/view.tsx";
import { useShell } from "../../../shell.tsx";
import { useHost } from "./domain.tsx";

/** HostTabPage is a plugin's place on a host, at /hosts/:name/tab/:plugin/:tab. */
export function HostTabPage(): JSX.Element {
  const shell = useShell();
  const domain = useHost();
  const params = useParams<{ plugin: string; tab: string }>();
  const entry = () =>
    shell.plugins.hostTabs().find((candidate) => candidate.plugin === params.plugin && candidate.id === params.tab);
  return (
    <View id={`host-tab-${params.plugin}-${params.tab}`} crumbs={domain.crumbs(entry()?.label ?? params.tab ?? "")}>
      <Show
        when={entry()}
        fallback={
          <EmptyState icon="puzzle" title={shell.plugins.loading() ? "Loading…" : "Not installed"} />
        }
      >
        {/* Keyed by host, so moving to another host mounts the plugin's view afresh. */}
        <For each={[domain.name()]}>
          {(host) => <HostTabMount half={entry()!.half} id={entry()!.id} host={host} />}
        </For>
      </Show>
    </View>
  );
}

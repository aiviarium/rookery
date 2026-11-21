// The route a plugin's page answers, and the page a path no plugin claims is.
import { Match, Switch } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useLocation } from "@solidjs/router";
import { Button, EmptyState } from "@aiviarium/nocturne";

import { useShell } from "../shell.tsx";
import { View } from "../parts/view.tsx";
import { ClientHalfMount } from "./mount.tsx";

/**
 * PluginRoute resolves the current path against the pages the installed client
 * halves declared. A path under a declared route is the same page, so a plugin
 * may route inside it.
 */
export function PluginRoute() {
  const shell = useShell();
  const location = useLocation();

  const mount = () => {
    const path = location.pathname;
    return shell.plugins
      .mounts()
      .find((candidate) => path === candidate.route || path.startsWith(`${candidate.route}/`));
  };

  return (
    <Switch
      fallback={<NotFound path={location.pathname} />}
    >
      <Match when={mount()}>
        {found => <ClientHalfMount half={found().half} map={found().kind} id={found().id} />}
      </Match>
    </Switch>
  );
}

/** NotFound is a path nothing answers, with the way home. */
export function NotFound(props: { path: string }): JSX.Element {
  return (
    <View id="not-found" crumbs={[{ label: "Not found" }]}>
      <EmptyState
        title="This page does not exist"
        description={`Nothing on this server answers ${props.path}. A plugin may have been removed, or the address may have a typo.`}
      >
        <Button
          variant="primary"
          onClick={() => {
            location.assign("/");
          }}
        >
          Go home
        </Button>
      </EmptyState>
    </View>
  );
}

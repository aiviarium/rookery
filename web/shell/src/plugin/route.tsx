// The route a plugin's page answers, and the page a path no plugin claims is.
import { Match, Switch } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useLocation } from "@solidjs/router";
import { Button, EmptyState } from "@aiviarium/nocturne";

import { useShell } from "../shell.tsx";
import { View } from "../parts/view.tsx";
import { ClientHalfMount } from "./mount.tsx";

/**
 * PluginRoute resolves the current path against the top-level views the
 * installed client halves declared. A path under a declared route is the same
 * page, so a plugin may route inside it. Views of a kind of space are places in
 * each space and answer there; admin pages answer in Administration.
 */
export function PluginRoute() {
  const shell = useShell();
  const location = useLocation();

  const mount = () => {
    const path = location.pathname;
    return shell.plugins
      .mounts()
      .find(
        (candidate) =>
          candidate.kind === "views" &&
          (candidate.space ?? "") === "" &&
          (path === candidate.route || path.startsWith(`${candidate.route}/`)),
      );
  };

  return (
    <Switch fallback={<NotFound />}>
      <Match when={mount()}>{(found) => <ClientHalfMount half={found().half} map={found().kind} id={found().id} />}</Match>
    </Switch>
  );
}

/** NotFound is a path nothing answers, with the way home. */
export function NotFound(): JSX.Element {
  const shell = useShell();
  return (
    <View id="not-found" crumbs={[{ label: "Not found" }]}>
      <EmptyState icon="search" title="Nothing here">
        <Button onClick={() => shell.navigate("/")}>Home</Button>
      </EmptyState>
    </View>
  );
}

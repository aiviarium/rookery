// Spaces: containers of work of a kind a plugin declares. At the top of the
// sidebar each space is a row that drills in; inside one are its overview, the
// places the plugins of its kind add, its documents and records, and its
// settings. Every page under /spaces/{id} is scoped to that space.
import { For, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { defineRoutes, useParams } from "@solidjs/router";

import { ClientHalfMount } from "../../plugin/mount.tsx";
import { NotFound } from "../../plugin/route.tsx";
import { useShell } from "../../shell.tsx";
import { spacePath } from "../../spaces.ts";
import { PreferencesPage } from "../settings/preferences.tsx";
import type { Area } from "../types.ts";
import { DataPage } from "./data.tsx";
import { DocumentsPage } from "./documents.tsx";
import { SpaceDomain, spaceCrumbs, useSpace } from "./domain.tsx";
import { SpaceOverview } from "./overview.tsx";
import { NewSpaceHost, SpaceSidebar, useSpacesGroup } from "./sidebar.tsx";

/** SpaceSettingsPage is the settings set for everyone in the space. */
function SpaceSettingsPage(): JSX.Element {
  const domain = useSpace();
  return <PreferencesPage space={domain.id} crumbs={spaceCrumbs(domain, "Settings")} />;
}

/** SpaceViewPage is a plugin's place in a space, under its declared route. */
function SpaceViewPage(): JSX.Element {
  const shell = useShell();
  const domain = useSpace();
  const params = useParams<{ rest: string }>();
  const views = () => shell.plugins.spaceViews(domain.space()?.kind ?? "");
  const found = () => {
    const path = `/${params.rest ?? ""}`;
    return views().find((view) => path === view.route || path.startsWith(`${view.route}/`));
  };
  // A path the plugin navigates to that is one of its space's views stays in the space.
  const at = (path: string) =>
    views().some((view) => path === view.route || path.startsWith(`${view.route}/`)) ? `${spacePath(domain.id)}${path}` : path;
  return (
    <Show when={domain.space() !== undefined}>
      <Show when={found()} fallback={<NotFound />}>
        <For each={[`${found()?.plugin}/${found()?.id}`]}>
          {() => {
            const view = found()!;
            return <ClientHalfMount half={view.half} map="views" id={view.id} at={at} />;
          }}
        </For>
      </Show>
    </Show>
  );
}

export const routes = defineRoutes([
  {
    path: "/spaces/:id",
    component: SpaceDomain,
    children: [
      { path: "/", component: SpaceOverview },
      { path: "/documents", component: DocumentsPage },
      { path: "/data", component: DataPage },
      { path: "/settings", component: SpaceSettingsPage },
      { path: "/*rest", component: SpaceViewPage },
    ],
  },
]);

/** SPACE_PATH matches an address inside a space, with its id. */
const SPACE_PATH = /^\/spaces\/([^/]+)(?:\/|$)/;

export const levels: Area["levels"] = [
  {
    id: "space",
    depth: 1,
    match: (path) => {
      const found = SPACE_PATH.exec(path);
      return found === null ? undefined : { id: decodeURIComponent(found[1] ?? "") };
    },
    Sidebar: SpaceSidebar,
  },
];

export const top: Area["top"] = [{ order: 20, use: useSpacesGroup }];

export const overlays: Area["overlays"] = [NewSpaceHost];

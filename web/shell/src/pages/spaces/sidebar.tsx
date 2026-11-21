// Spaces in the sidebar: a group at the top with each space, which drills into
// it, and the space's level: its overview, the places the plugins of its kind
// add, its documents and records, and its settings for those who may change
// them. The switcher moves to another space and keeps the page.
import { Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useLocation } from "@solidjs/router";
import { Mark } from "@aiviarium/nocturne";

import { type LevelProps, TOP } from "../../chrome/levels.ts";
import { LevelSidebar, type SidebarEntry, type SidebarGroup, useSiblings } from "../../chrome/sidebar.tsx";
import { createLive } from "../../parts/data.ts";
import { useShell } from "../../shell.tsx";
import { spacePath } from "../../spaces.ts";
import { NewSpace } from "./new.tsx";

/** useSpacesGroup is the Spaces group at the top of the sidebar. */
export function useSpacesGroup(): () => SidebarGroup[] {
  const shell = useShell();
  return () => {
    const spaces = shell.spaces.list();
    const canMake = shell.spaces.kinds().length > 0;
    if (spaces.length === 0 && !canMake) return [];
    return [
      {
        label: "Spaces",
        action: canMake ? { label: "New space", icon: "plus", onClick: () => openNewSpace() } : undefined,
        items: spaces.map((space) => ({ label: space.name, icon: "layers" as const, drill: { id: "space", params: { id: space.id } } })),
      },
    ];
  };
}

// Whether the new space dialog is open. The chrome draws it as one of the
// areas' overlays, so the group's plus opens it over any page.
const [newSpaceOpen, setNewSpaceOpen] = createSignal(false);

/** openNewSpace opens the dialog that makes a space. */
export function openNewSpace(): void {
  setNewSpaceOpen(true);
}

/** NewSpaceHost draws the new space dialog wherever the chrome puts it. */
export function NewSpaceHost(): JSX.Element {
  const shell = useShell();
  return (
    <Show when={newSpaceOpen()}>
      <NewSpace
        onClose={() => setNewSpaceOpen(false)}
        onCreated={(id) => {
          setNewSpaceOpen(false);
          void shell.spaces.refresh();
          shell.navigate(spacePath(id));
        }}
      />
    </Show>
  );
}

/** SpaceSidebar is the space level's sidebar. */
export function SpaceSidebar(props: LevelProps): JSX.Element {
  const shell = useShell();
  const location = useLocation();
  const id = () => props.params.id ?? "";
  const space = () => shell.spaces.list().find((one) => one.id === id());
  // The settings place shows to those who may set a value for the space.
  const settings = createLive(() => (props.inside ? shell.rookery.settings.list({}) : Promise.resolve(undefined)).catch(() => undefined));
  const settable = () => (settings()?.settings ?? []).some((setting) => setting.editable.includes("space"));

  const place = () => {
    if (!props.inside) return undefined;
    const rest = location.pathname.slice(spacePath(id()).length).replace(/^\//, "");
    return rest;
  };
  const item = (label: string, at: string, icon: SidebarEntry["icon"]): SidebarEntry => {
    const here = place();
    return { label, icon, href: spacePath(id(), at), current: here !== undefined && (here === at || (at !== "" && here.startsWith(`${at}/`))) };
  };

  const groups = (): SidebarGroup[] => {
    const kind = space()?.kind ?? "";
    const views = shell.plugins.spaceViews(kind).map((view) => item(view.label, view.route.replace(/^\//, ""), "puzzle"));
    const own: SidebarEntry[] = [item("Documents", "documents", "file"), item("Datastore", "data", "hard-drive")];
    if (settable()) own.push(item("Settings", "settings", "settings"));
    return [{ items: [item("Overview", "", "layers"), ...views] }, { items: own }];
  };

  const switcher = useSiblings({
    label: "Switch space",
    items: () => shell.spaces.list().map((one) => ({ id: one.id, label: one.name })),
    current: id,
    inside: () => props.inside,
    path: (other) => spacePath(other, place() ?? ""),
    level: (other) => ({ id: "space", params: { id: other } }),
  });

  const name = () => space()?.name ?? "Space";
  return (
    <LevelSidebar
      label={`${name()}'s places`}
      back={{ label: "Rookery", to: { id: TOP } }} path={[{ label: "Rookery", to: { id: TOP }, show: "hidden" }]}
      title={name()}
      mark={<Mark name={name()} hue="accent" decorative />}
      switcher={switcher()}
      groups={groups()}
    />
  );
}

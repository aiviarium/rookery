// Settings' level: all settings, then one place per section that has a
// setting the person may change at the scope. The scope stays as places change.
import type { JSX } from "@solidjs/web";
import { useLocation } from "@solidjs/router";
import { Count } from "@aiviarium/nocturne";

import { type LevelProps, TOP } from "../../chrome/levels.ts";
import { LevelSidebar, type SidebarEntry } from "../../chrome/sidebar.tsx";
import { useSettings } from "./preferences.tsx";
import { CORE, editableAt, modifiedAt, sectionPath, useScope } from "./scope.ts";

/** SettingsSidebar is the settings level's sidebar. */
export function SettingsSidebar(_props: LevelProps): JSX.Element {
  const location = useLocation();
  const { scope, query } = useScope();
  const { settings, sections, titleOf } = useSettings();

  const here = (plugin?: string) =>
    (settings()?.settings ?? []).filter((setting) => (plugin === undefined || setting.plugin === plugin) && editableAt(setting, scope()));
  const meta = (plugin?: string) => {
    const modified = here(plugin).filter((setting) => modifiedAt(setting, scope())).length;
    return modified > 0 ? <Count value={modified} /> : undefined;
  };

  const items = (): SidebarEntry[] => [
    { label: "All settings", icon: "settings", href: `${sectionPath()}${query()}`, current: location.pathname === "/settings", meta: meta() },
    ...sections()
      .filter((plugin) => here(plugin).length > 0)
      .map(
        (plugin): SidebarEntry => ({
          label: titleOf(plugin),
          icon: plugin === CORE ? "sliders-horizontal" : "puzzle",
          href: `${sectionPath(plugin)}${query()}`,
          current: location.pathname === sectionPath(plugin),
          meta: meta(plugin),
        }),
      ),
  ];

  return <LevelSidebar label="Settings" title="Settings" back={{ label: "Rookery", to: { id: TOP } }} groups={[{ items: items() }]} />;
}

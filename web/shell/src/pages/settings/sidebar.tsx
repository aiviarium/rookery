// The sidebar inside Settings: back home, then all settings and one place per
// section with the number modified at the scope shown, then, for admins, the
// server's secrets and connections. The scope stays as the places change.
import type { JSX } from "@solidjs/web";
import { useLocation } from "@solidjs/router";
import { Count, type NavItemProps } from "@aiviarium/nocturne";

import { DomainSidebar } from "../../chrome/sidebar.tsx";
import { useShell } from "../../shell.tsx";
import { useSettings } from "./preferences.tsx";
import { CORE, modifiedAt, sectionPath, useScope } from "./scope.ts";

/** SettingsSidebar is the settings domain's sidebar. */
export function SettingsSidebar(): JSX.Element {
  const shell = useShell();
  const location = useLocation();
  const { scope, query } = useScope();
  const { settings, sections, titleOf } = useSettings();

  const count = (plugin?: string) =>
    (settings()?.settings ?? []).filter((setting) => (plugin === undefined || setting.plugin === plugin) && modifiedAt(setting, scope())).length;
  const meta = (value: number) => (value > 0 ? <Count value={value} /> : undefined);

  const groups = () => {
    const preferences: NavItemProps[] = [
      { label: "All settings", icon: "settings", href: `${sectionPath()}${query()}`, current: location.pathname === "/settings", meta: meta(count()) },
      ...sections().map(
        (plugin): NavItemProps => ({
          label: titleOf(plugin),
          icon: plugin === CORE ? "sliders-horizontal" : "puzzle",
          href: `${sectionPath(plugin)}${query()}`,
          current: location.pathname === sectionPath(plugin),
          meta: meta(count(plugin)),
        }),
      ),
    ];
    const out: Array<{ label?: string; items: NavItemProps[] }> = [{ label: "Preferences", items: preferences }];
    if (shell.signIn.user()?.admin === true) {
      out.push({
        label: "Server",
        items: [
          { label: "Secrets", icon: "lock", href: "/settings/secrets", current: location.pathname === "/settings/secrets" },
          { label: "Connections", icon: "link", href: "/settings/connections", current: location.pathname === "/settings/connections" },
        ],
      });
    }
    return out;
  };

  return <DomainSidebar label="Settings" back={{ label: "Home", href: "/" }} title="Settings" groups={groups()} />;
}

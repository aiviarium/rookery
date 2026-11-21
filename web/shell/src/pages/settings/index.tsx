// Settings: the values a person sets for themselves and, where they may, for
// everyone. A space's own values are in the space.
import type { JSX } from "@solidjs/web";
import { defineRoutes, useParams } from "@solidjs/router";

import { prefixMatch } from "../../chrome/levels.ts";
import type { Area } from "../types.ts";
import { PreferencesPage } from "./preferences.tsx";
import { SettingsSidebar } from "./sidebar.tsx";

function SectionPage(): JSX.Element {
  const params = useParams<{ plugin: string }>();
  return <PreferencesPage plugin={params.plugin} />;
}

export const routes = defineRoutes([
  { path: "/settings", component: () => <PreferencesPage /> },
  { path: "/settings/p/:plugin", component: SectionPage },
]);

export const levels: Area["levels"] = [{ id: "settings", depth: 1, match: prefixMatch("/settings"), Sidebar: SettingsSidebar }];

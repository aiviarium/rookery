// The server's own pages: people and access, spaces, the deployment's settings,
// keys, the tailnet, plugins, the datastore, documents and the server's status.
import { defineRoutes } from "@solidjs/router";

import type { Area } from "../types.ts";
import { DataPage } from "./data.tsx";
import { DocumentsPage } from "./documents.tsx";
import { KeysPage } from "./keys.tsx";
import { PeoplePage } from "./people.tsx";
import { PluginsPage } from "./plugins.tsx";
import { SettingsPage } from "./settings.tsx";
import { SpacesPage } from "./spaces.tsx";
import { ServerStatusPage } from "./status.tsx";
import { TailnetPage } from "./tailnet.tsx";

export const routes = defineRoutes([
  { path: "/people", component: PeoplePage },
  { path: "/spaces", component: SpacesPage },
  { path: "/settings", component: SettingsPage },
  { path: "/keys", component: KeysPage },
  { path: "/tailnet", component: TailnetPage },
  { path: "/plugins", component: PluginsPage },
  { path: "/data", component: DataPage },
  { path: "/documents", component: DocumentsPage },
  { path: "/status", component: ServerStatusPage },
]);

export const nav: Area["nav"] = [
  { label: "People", href: "/people", group: "Administration", icon: "groups" },
  { label: "Spaces", href: "/spaces", group: "Administration", icon: "space" },
  { label: "Settings", href: "/settings", group: "Administration", icon: "settings" },
  { label: "Keys", href: "/keys", group: "Administration", icon: "key" },
  { label: "Tailnet", href: "/tailnet", group: "Administration", icon: "tailnet" },
  { label: "Plugins", href: "/plugins", group: "Administration", icon: "plugin" },
  { label: "Datastore", href: "/data", group: "Data", icon: "database" },
  { label: "Documents", href: "/documents", group: "Data", icon: "document" },
  { label: "Server", href: "/status", group: "Data", icon: "gauge" },
];

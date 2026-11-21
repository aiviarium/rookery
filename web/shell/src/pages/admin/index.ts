// The server's own pages: people and access, spaces, the deployment's settings,
// keys, the tailnet, plugins, the datastore, documents and the server's status.
import { defineRoutes } from "@solidjs/router";

import type { Area } from "../types.ts";
import { DataPage } from "./data.tsx";
import { DocumentsPage } from "./documents.tsx";
import { KeysPage } from "./keys.tsx";
import { PeoplePage } from "./people.tsx";
import { PluginsPage } from "./plugins.tsx";
import { SpacesPage } from "./spaces.tsx";
import { ServerStatusPage } from "./status.tsx";
import { TailnetPage } from "./tailnet.tsx";

export const routes = defineRoutes([
  { path: "/people", component: PeoplePage },
  { path: "/spaces", component: SpacesPage },
  { path: "/keys", component: KeysPage },
  { path: "/tailnet", component: TailnetPage },
  { path: "/plugins", component: PluginsPage },
  { path: "/data", component: DataPage },
  { path: "/documents", component: DocumentsPage },
  { path: "/status", component: ServerStatusPage },
]);

export const nav: Area["nav"] = [
  { label: "People", href: "/people", group: "Administration", icon: "users" },
  { label: "Spaces", href: "/spaces", group: "Administration", icon: "building-2" },
  { label: "Keys", href: "/keys", group: "Administration", icon: "key-round" },
  { label: "Tailnet", href: "/tailnet", group: "Administration", icon: "link" },
  { label: "Plugins", href: "/plugins", group: "Administration", icon: "puzzle" },
  { label: "Datastore", href: "/data", group: "Data", icon: "hard-drive" },
  { label: "Documents", href: "/documents", group: "Data", icon: "file" },
  { label: "Server", href: "/status", group: "Data", icon: "activity" },
];

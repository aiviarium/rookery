// Administration: people and access, the server and what is installed on it,
// its secrets and connections, and every record and document. The account menu
// opens it for admins; inside, the sidebar holds its places.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { defineRoutes, useParams } from "@solidjs/router";

import { prefixMatch } from "../../chrome/levels.ts";
import { View } from "../../parts/view.tsx";
import { ClientHalfMount } from "../../plugin/mount.tsx";
import { NotFound } from "../../plugin/route.tsx";
import { useShell } from "../../shell.tsx";
import { DataPage } from "../spaces/data.tsx";
import { DocumentsPage } from "../spaces/documents.tsx";
import { SpacesPage } from "../spaces/manage.tsx";
import type { Area } from "../types.ts";
import { ConnectionsSection } from "./connections.tsx";
import { KeysPage } from "./keys.tsx";
import { ADMIN, AccessPage, GroupsPage, PeoplePage } from "./people.tsx";
import { PluginsPage } from "./plugins.tsx";
import { SecretsSection } from "./secrets.tsx";
import { AdminSidebar, adminPluginPath } from "./sidebar.tsx";
import { ServerStatusPage } from "./status.tsx";
import { TailnetPage } from "./tailnet.tsx";

function SecretsPage(): JSX.Element {
  const [adding, setAdding] = createSignal(false);
  const [version, setVersion] = createSignal(0);
  return (
    <View
      id="secrets"
      crumbs={[ADMIN, { label: "Secrets" }]}
      actions={[
        { id: "add", label: "Add secret", icon: "plus", onClick: () => setAdding(true) },
        { id: "refresh", label: "Refresh", icon: "refresh-cw", overflow: true, onClick: () => setVersion((v) => v + 1) },
      ]}
    >
      <SecretsSection version={version()} adding={adding()} onAdd={() => setAdding(true)} onCloseAdd={() => setAdding(false)} />
    </View>
  );
}

function ConnectionsPage(): JSX.Element {
  const [adding, setAdding] = createSignal(false);
  const [version, setVersion] = createSignal(0);
  return (
    <View
      id="connections"
      crumbs={[ADMIN, { label: "Connections" }]}
      actions={[
        { id: "add", label: "Add connection", icon: "plus", onClick: () => setAdding(true) },
        { id: "refresh", label: "Refresh", icon: "refresh-cw", overflow: true, onClick: () => setVersion((v) => v + 1) },
      ]}
    >
      <ConnectionsSection version={version()} adding={adding()} onAdd={() => setAdding(true)} onCloseAdd={() => setAdding(false)} />
    </View>
  );
}

/** PluginAdminPage is a page a plugin adds to Administration. */
function PluginAdminPage(): JSX.Element {
  const shell = useShell();
  const params = useParams<{ plugin: string; page: string }>();
  const found = () => shell.plugins.adminPages().find((page) => page.plugin === params.plugin && page.id === params.page);
  // A path the plugin navigates to that is one of its admin pages stays in Administration.
  const at = (path: string) => {
    const page = shell.plugins.adminPages().find((one) => path === one.route || path.startsWith(`${one.route}/`));
    return page === undefined ? path : `${adminPluginPath(page.plugin, page.id)}${path.slice(page.route.length)}`;
  };
  return (
    <Show when={found()} fallback={<Show when={!shell.plugins.loading()}><NotFound /></Show>}>
      <For each={[`${params.plugin}/${params.page}`]}>
        {() => {
          const page = found()!;
          return (
            <View id={`admin-${page.plugin}-${page.id}`} crumbs={[ADMIN, { label: page.label }]}>
              <ClientHalfMount half={page.half} map="adminPages" id={page.id} at={at} />
            </View>
          );
        }}
      </For>
    </Show>
  );
}

export const routes = defineRoutes([
  { path: "/admin", component: PeoplePage },
  { path: "/admin/people", component: PeoplePage },
  { path: "/admin/groups", component: GroupsPage },
  { path: "/admin/access", component: AccessPage },
  { path: "/admin/plugins", component: PluginsPage },
  { path: "/admin/spaces", component: SpacesPage },
  { path: "/admin/keys", component: KeysPage },
  { path: "/admin/tailnet", component: TailnetPage },
  { path: "/admin/server", component: ServerStatusPage },
  { path: "/admin/secrets", component: SecretsPage },
  { path: "/admin/connections", component: ConnectionsPage },
  { path: "/admin/data", component: DataPage },
  { path: "/admin/documents", component: DocumentsPage },
  { path: "/admin/p/:plugin/:page/*", component: PluginAdminPage },
]);

export const levels: Area["levels"] = [{ id: "admin", depth: 1, match: prefixMatch("/admin"), Sidebar: AdminSidebar }];

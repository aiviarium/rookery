// Settings: everyone's preferences at a scope, and for admins the server's
// secrets and connections. Inside /settings the sidebar drills into its
// places: all settings, one section per plugin, then the server's pages.
import { createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { defineRoutes, useParams } from "@solidjs/router";

import { View } from "../../parts/view.tsx";
import type { Area } from "../types.ts";
import { ConnectionsSection } from "./connections.tsx";
import { PreferencesPage } from "./preferences.tsx";
import { SecretsSection } from "./secrets.tsx";
import { SettingsSidebar } from "./sidebar.tsx";

function SectionPage(): JSX.Element {
  const params = useParams<{ plugin: string }>();
  return <PreferencesPage plugin={params.plugin} />;
}

function SecretsPage(): JSX.Element {
  const [adding, setAdding] = createSignal(false);
  const [version, setVersion] = createSignal(0);
  return (
    <View
      id="secrets"
      crumbs={[{ label: "Settings", href: "/settings" }, { label: "Secrets" }]}
      actions={[
        { id: "refresh", label: "Refresh", icon: "refresh-cw", overflow: true, onClick: () => setVersion((v) => v + 1) },
        { id: "add", label: "Add secret", icon: "plus", primary: true, onClick: () => setAdding(true) },
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
      crumbs={[{ label: "Settings", href: "/settings" }, { label: "Connections" }]}
      actions={[
        { id: "refresh", label: "Refresh", icon: "refresh-cw", overflow: true, onClick: () => setVersion((v) => v + 1) },
        { id: "add", label: "Add connection", icon: "plus", primary: true, onClick: () => setAdding(true) },
      ]}
    >
      <ConnectionsSection version={version()} adding={adding()} onAdd={() => setAdding(true)} onCloseAdd={() => setAdding(false)} />
    </View>
  );
}

export const routes = defineRoutes([
  { path: "/settings", component: () => <PreferencesPage /> },
  { path: "/settings/p/:plugin", component: SectionPage },
  { path: "/settings/secrets", component: SecretsPage },
  { path: "/settings/connections", component: ConnectionsPage },
]);

export const nav: Area["nav"] = [{ label: "Settings", href: "/settings", icon: "settings" }];

export const domains: Area["domains"] = [{ pattern: "/settings/*", Sidebar: SettingsSidebar }];

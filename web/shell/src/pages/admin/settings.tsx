// The deployment's knobs: settings, secrets and connections.
import { Match, Switch, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Tabs } from "@aiviarium/nocturne";

import { Page } from "../../parts/page.tsx";
import { ConnectionsSection } from "./connections.tsx";
import { SecretsSection } from "./secrets.tsx";
import { SettingsSection } from "./settings-section.tsx";

/** TABS are the three kinds of knob. */
const TABS = [
  { id: "settings", label: "Settings" },
  { id: "secrets", label: "Secrets" },
  { id: "connections", label: "Connections" },
];

/** SettingsPage is settings, secrets and connections, one tab each. */
export function SettingsPage(): JSX.Element {
  const [tab, setTab] = createSignal("settings");

  return (
    <Page title="Settings" description="What this deployment is configured with.">
      <Tabs label="Settings sections" idPrefix="settings-tabs" items={TABS} value={tab()} onChange={setTab} />
      <Switch>
        <Match when={tab() === "settings"}>
          <SettingsSection />
        </Match>
        <Match when={tab() === "secrets"}>
          <SecretsSection />
        </Match>
        <Match when={tab() === "connections"}>
          <ConnectionsSection />
        </Match>
      </Switch>
    </Page>
  );
}

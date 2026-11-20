// The deployment's knobs: settings, secrets and connections.
import { Match, Show, Switch, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";

import { useShell } from "../../shell.tsx";
import { Page, Tabs } from "../../ui.tsx";
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
  const shell = useShell();
  const [tab, setTab] = createSignal("settings");
  void shell;

  return (
    <Page title="Settings" description="What this deployment is configured with.">
      <Tabs active={tab()} tabs={TABS} onSelect={setTab} />
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
      <Show when={false}>{null}</Show>
    </Page>
  );
}

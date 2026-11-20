// The bottom line: whether the shell is talking to the server, and where it is.
import type { JSX } from "@solidjs/web";
import { MetaLine, State, type DotStatus } from "@aiviarium/nocturne";

import { useShell } from "../shell.tsx";

/** StatusBar is the link to the server and what this shell is showing. */
export function StatusBar(): JSX.Element {
  const shell = useShell();

  const status = () => shell.connection.status();

  const label = (): string => {
    switch (status()) {
      case "online":
        return "Connected";
      case "connecting":
        return "Connecting…";
      default:
        return "Offline";
    }
  };

  const dot = (): DotStatus => {
    switch (status()) {
      case "online":
        return "live";
      case "connecting":
        return "warn";
      default:
        return "error";
    }
  };

  const halves = () => shell.plugins.halves().length;

  return (
    <>
      <span title="The shell's stream to the server">
        <State status={dot()}>{label()}</State>
      </span>
      <MetaLine
        class="shell-status__where"
        items={[
          { label: shell.rookery.baseUrl, icon: "server", mono: true },
          shell.spaces.currentSpace()?.name ?? false,
          `${halves()} plugin${halves() === 1 ? "" : "s"}`,
        ]}
      />
    </>
  );
}

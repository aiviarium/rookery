// mount points a client half at an element, and takes it down with its owner.
import { onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useNavigate } from "@solidjs/router";
import { clientContext, type LoadedClientHalf } from "@rookery/client";

import { useShell } from "../shell.tsx";

/** MountProps names the half and the page or tab it draws. */
export interface MountProps {
  /** half is the plugin's client half. */
  readonly half: LoadedClientHalf;
  /** map is which of the half's maps holds the mount. */
  readonly map: "views" | "adminPages";
  /** id is the mount's id inside the map. */
  readonly id: string;
  /** host is the host a host tab is about. */
  readonly host?: string;
  /** at maps a path the plugin navigates to onto the shell's, such as into a space. */
  readonly at?: (path: string) => string;
}

/**
 * ClientHalfMount gives a client half an element and its context, and runs what
 * it returns when the page goes.
 */
export function ClientHalfMount(props: MountProps): JSX.Element {
  const shell = useShell();
  const navigate = useNavigate();
  let element: HTMLDivElement | undefined;

  onSettled(() => {
    if (element === undefined) return;
    const context = clientContext(shell.rookery, props.half, (path) => navigate(props.at === undefined ? path : props.at(path)));
    const mounts = props.map === "views" ? props.half.half.views : props.half.half.adminPages;
    const mount = mounts?.[props.id];
    if (mount === undefined) {
      element.textContent = `${props.id}: nothing to draw`;
      return;
    }
    const remove = mount(context, element);
    return () => {
      if (typeof remove === "function") remove();
    };
  });

  return <div class="nc-stack" ref={(node) => { element = node; }} />;
}

/**
 * HostTabMount gives a client half the element of a tab on a host's page.
 */
export function HostTabMount(props: { half: LoadedClientHalf; id: string; host: string }): JSX.Element {
  const shell = useShell();
  const navigate = useNavigate();
  let element: HTMLDivElement | undefined;

  onSettled(() => {
    if (element === undefined) return;
    const context = clientContext(shell.rookery, props.half, (path) => navigate(path));
    const mount = props.half.half.hostTabs?.[props.id];
    if (mount === undefined) {
      element.textContent = `${props.id}: nothing to draw`;
      return;
    }
    const remove = mount(context, element, props.host);
    return () => {
      if (typeof remove === "function") remove();
    };
  });

  return <div class="nc-stack" ref={(node) => { element = node; }} />;
}

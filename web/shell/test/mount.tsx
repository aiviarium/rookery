// Mounting the whole shell the way main.tsx does, against a made-up server.
import { flush } from "solid-js";
import { render } from "@solidjs/web";
import type { AuthConfig, Rookery } from "@rookery/client";

import { App } from "../src/app.tsx";

/** Mounted is a rendered shell, and what a test asks it. */
export interface Mounted {
  readonly container: HTMLElement;
  /** text is everything the shell draws. */
  text(): string;
  /** find is the first element that matches a selector. */
  find(selector: string): HTMLElement | null;
  /** findAll is every element that matches a selector. */
  findAll(selector: string): HTMLElement[];
  /** click clicks the first element that matches, and settles. */
  click(selector: string): Promise<void>;
  /** clickElement clicks an element a test found, and settles. */
  clickElement(element: HTMLElement): Promise<void>;
  /** type writes into the first input that matches, and settles. */
  type(selector: string, value: string): Promise<void>;
  /** unmount takes the shell down. */
  unmount(): void;
}

/** settle lets the shell's promises resolve and Solid write what they changed. */
export async function settle(times = 6): Promise<void> {
  for (let i = 0; i < times; i += 1) {
    await Promise.resolve();
    flush();
    await new Promise((resolve) => setTimeout(resolve, 0));
    flush();
  }
}

/** mount renders the shell on a client and a made-up sign-in configuration. */
export async function mount(
  client: Rookery,
  config: AuthConfig = { identityProvider: false },
  path?: string,
): Promise<Mounted> {
  // A test that names a path gets it; one that does not keeps the URL it set.
  if (path !== undefined) history.pushState({}, "", path);
  const container = document.createElement("div");
  document.body.append(container);
  const dispose = render(() => <App client={client} config={config} />, container);
  await settle();

  const find = (selector: string) => container.querySelector<HTMLElement>(selector);
  return {
    container,
    text: () => container.textContent ?? "",
    find,
    findAll: (selector) => [...container.querySelectorAll<HTMLElement>(selector)],
    click: async (selector) => {
      const element = find(selector);
      if (element === null) throw new Error(`no element matches ${selector}`);
      element.click();
      await settle();
    },
    clickElement: async (element) => {
      element.click();
      await settle();
    },
    type: async (selector, value) => {
      const element = find(selector);
      if (!(element instanceof HTMLInputElement)) throw new Error(`no input matches ${selector}`);
      element.value = value;
      element.dispatchEvent(new InputEvent("input", { bubbles: true }));
      await settle();
    },
    unmount: () => {
      dispose();
      container.remove();
    },
  };
}

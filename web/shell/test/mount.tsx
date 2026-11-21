// Mounting the whole shell the way main.tsx does, against a made-up server.
import { flush } from "solid-js";
import { render } from "@solidjs/web";
import type { AuthConfig, Rookery } from "@rookery/client";

import { App } from "../src/app.tsx";

/** Mounted is a rendered shell, and what a test asks it. */
export interface Mounted {
  readonly container: HTMLElement;
  /** text is everything the shell draws, including what it puts in an overlay. */
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
  /** choose opens the first Select that matches, and picks the option with this label. */
  choose(selector: string, label: string): Promise<void>;
  /** pick picks the option with this label of an open menu. */
  pick(label: string): Promise<void>;
  /** action clicks the view header's action with this id, opening the overflow menu when it is in there. */
  action(id: string): Promise<void>;
  /** fill writes into the field whose label says this. */
  fill(label: string, value: string): Promise<void>;
  /** select picks an option of the Select whose label says this. */
  select(label: string, option: string): Promise<void>;
  /** button clicks the first button whose text is this. */
  button(text: string): Promise<void>;
  /** submit clicks the open dialog's submit or confirm button. */
  submit(): Promise<void>;
  /** rowMenu opens the menu named "Actions for <name>" and picks an item. */
  rowMenu(name: string, label: string): Promise<void>;
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
  // A test that names a path gets it, and gets the URL back when the shell goes;
  // one that does not keeps the URL it set itself.
  const started = `${location.pathname}${location.search}`;
  if (path !== undefined) history.pushState({}, "", path);
  const container = document.createElement("div");
  document.body.append(container);
  const dispose = render(() => <App client={client} config={config} />, container);
  await settle();

  // Popovers, dialogs and menus mount in an overlay beside the app root, so a
  // test looks at the document, not only at the container.
  const find = (selector: string) => document.querySelector<HTMLElement>(selector);
  const pick = async (label: string) => {
    const item = [...document.querySelectorAll<HTMLElement>(".nc-menu__item")].find((candidate) =>
      candidate.textContent?.includes(label),
    );
    if (item === undefined) throw new Error(`no menu item says ${label}`);
    item.click();
    await settle();
  };
  const write = async (element: HTMLInputElement | HTMLTextAreaElement, value: string) => {
    element.value = value;
    element.dispatchEvent(new InputEvent("input", { bubbles: true }));
    element.dispatchEvent(new Event("change", { bubbles: true }));
    await settle();
  };
  const fieldFor = (label: string): HTMLInputElement | HTMLTextAreaElement => {
    for (const candidate of document.querySelectorAll<HTMLLabelElement>("label[for]")) {
      if (candidate.textContent?.trim() !== label) continue;
      const element = document.getElementById(candidate.htmlFor);
      if (element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement) return element;
    }
    throw new Error(`no field is labelled ${label}`);
  };
  const clickButton = async (text: string) => {
    const element = [...document.querySelectorAll<HTMLElement>("button")].find((candidate) => candidate.textContent?.trim() === text);
    if (element === undefined) throw new Error(`no button says ${text}`);
    element.click();
    await settle();
  };
  return {
    container,
    action: async (id) => {
      const direct = find(`[data-part='view-actions'] button[data-action='${id}']`);
      if (direct !== null) {
        direct.click();
        await settle();
        return;
      }
      const more = find("[data-part='view-overflow']");
      if (more === null) throw new Error(`no action ${id}`);
      more.click();
      await settle();
      const item = find(`[data-action='${id}']`);
      if (item === null) throw new Error(`no action ${id}`);
      item.click();
      await settle();
    },
    fill: async (label, value) => write(fieldFor(label), value),
    select: async (label, option) => {
      const select = [...document.querySelectorAll<HTMLElement>("[data-part='select']")].find(
        (candidate) => candidate.querySelector(".nc-field__label")?.textContent?.trim() === label,
      );
      const trigger = select?.querySelector<HTMLElement>("button");
      if (trigger === undefined || trigger === null) throw new Error(`no select is labelled ${label}`);
      trigger.click();
      await settle();
      await pick(option);
    },
    button: clickButton,
    submit: async () => {
      const submit = [...document.querySelectorAll<HTMLElement>("[data-part='dialog-submit'], [data-part='dialog-confirm']")].at(-1);
      if (submit === undefined) throw new Error("no dialog is open");
      submit.click();
      await settle();
    },
    rowMenu: async (name, label) => {
      const trigger = find(`button[aria-label='Actions for ${name}']`);
      if (trigger === null) throw new Error(`no menu for ${name}`);
      trigger.click();
      await settle();
      await pick(label);
    },
    text: () => document.body.textContent ?? "",
    find,
    findAll: (selector) => [...document.querySelectorAll<HTMLElement>(selector)],
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
    choose: async (selector, label) => {
      const element = find(selector);
      if (element === null) throw new Error(`no element matches ${selector}`);
      element.click();
      await settle();
      await pick(label);
    },
    pick,
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
      if (path !== undefined) history.pushState({}, "", started);
    },
  };
}

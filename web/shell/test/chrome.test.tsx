// The chrome around the pages: the inbox, drilling into a space and the status bar.
import { describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { EventSchema, type Notification } from "@rookery/proto";

import { ANOTHER_SPACE, A_SPACE, STATIC_USER, fakeClient } from "./fakes.ts";
import { mount } from "./mount.tsx";

/** unread is one notification nobody has read. */
function unread(): Notification {
  return {
    id: "01hnote",
    userId: STATIC_USER.id,
    title: "A job failed",
    body: "host.apply on studio",
    link: "",
    resource: "host://studio",
    read: false,
    actionsCbor: new Uint8Array(),
    answersCbor: new Uint8Array(),
  } as Notification;
}

describe("the chrome", () => {
  test("counts unread notifications in the sidebar and marks them read in the inbox", async () => {
    const view = await mount(fakeClient({ user: STATIC_USER, notifications: [unread()] }), { identityProvider: false }, "/inbox");
    try {
      expect(view.find("a[href='/inbox']")?.textContent).toContain("1");
      expect(view.text()).toContain("A job failed");
      await view.action("read");
      expect(view.find("a[href='/inbox']")?.textContent).not.toContain("1");
    } finally {
      view.unmount();
    }
  });

  test("tiles pages as tabs: the address and the sidebar follow the focused one", async () => {
    const view = await mount(fakeClient({ user: STATIC_USER }), { identityProvider: false }, "/inbox");
    const frames = () => view.findAll("[data-dock-frame]");
    const focusedTab = () =>
      document.querySelector(`[data-part=frame][data-focused] [data-dock-tab]:not([hidden])`)?.getAttribute("data-dock-tab");
    try {
      expect(frames()).toHaveLength(1);
      expect(view.findAll("[data-page-tab]")).toHaveLength(1);

      // A split opens the page beside itself, focused.
      await view.click("[data-part=dock-split-right]");
      expect(frames()).toHaveLength(2);
      expect(location.pathname).toBe("/inbox");
      const second = focusedTab();
      expect(second).toBeTruthy();

      // The sidebar moves the focused tab alone, and the address with it.
      await view.click("a[href='/']");
      expect(location.pathname).toBe("/");
      const [left, right] = frames();
      expect(left?.textContent).toContain("Inbox");
      expect(right?.querySelector(`[data-dock-tab='${second}']`)).not.toBeNull();

      // Focusing the other frame brings its tab's address back.
      left!.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
      await view.click("[data-dock-frame] [data-page-tab]");
      expect(location.pathname).toBe("/inbox");

      // Closing a frame's only tab closes the frame, and the other fills the row.
      await view.clickElement(right!.querySelector("[data-part=tab-strip-close], [aria-label^='Close']") as HTMLElement);
      expect(frames()).toHaveLength(1);
    } finally {
      view.unmount();
    }
  });

  test("drills into a space and scopes every call to it", async () => {
    const client = fakeClient({ user: STATIC_USER, spaces: [A_SPACE, ANOTHER_SPACE] });
    const view = await mount(client, { identityProvider: false }, "/");
    try {
      // The space's row drills the sidebar in and leaves the page as it is.
      await view.button("Analysis");
      expect(location.pathname).toBe("/");
      expect(view.find("[data-part='sidebar-switcher']")).not.toBeNull();
      expect(client.space()).toBe("");

      // A place in the space opens it, scoped to the space.
      await view.click(`a[href='/spaces/${A_SPACE.id}']`);
      expect(location.pathname).toBe(`/spaces/${A_SPACE.id}`);
      expect(client.space()).toBe(A_SPACE.id);

      // The switcher keeps the place and moves to the other space.
      await view.click("[data-part='sidebar-switcher']");
      await view.pick(ANOTHER_SPACE.name);
      expect(location.pathname).toBe(`/spaces/${ANOTHER_SPACE.id}`);
      expect(client.space()).toBe(ANOTHER_SPACE.id);
    } finally {
      view.unmount();
    }
  });

  test("hides the sidebar and brings it back from the start of the page's header", async () => {
    const view = await mount(fakeClient({ user: STATIC_USER }), { identityProvider: false }, "/inbox");
    try {
      await view.click("[data-part='sidebar-header'] [data-part='sidebar-toggle']");
      expect(view.find("[data-part='app-shell']")?.getAttribute("data-sidebar")).toBe("hidden");
      const show = view.find("[data-part='view-header'] > [data-part='sidebar-toggle']");
      expect(show?.getAttribute("aria-label")).toBe("Show the sidebar");
      await view.click("[data-part='view-header'] > [data-part='sidebar-toggle']");
      expect(view.find("[data-part='app-shell']")?.hasAttribute("data-sidebar")).toBe(false);
    } finally {
      view.unmount();
    }
  });

  test("keeps the status bar quiet while the link is up", async () => {
    const view = await mount(
      fakeClient({
        user: STATIC_USER,
        events: [create(EventSchema, { name: "rookery/hosts.changed" })],
      }),
    );
    try {
      expect(view.find("[data-part='connection-state']")).toBeNull();
    } finally {
      view.unmount();
    }
  });
});

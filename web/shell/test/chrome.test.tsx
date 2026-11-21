// The chrome around the pages: the inbox, the space switcher and the status bar.
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

  test("switches the space every call is scoped to", async () => {
    const client = fakeClient({ user: STATIC_USER, spaces: [A_SPACE, ANOTHER_SPACE] });
    const view = await mount(client);
    try {
      await view.click("[data-part='sidebar-switcher']");
      await view.pick("Analysis");

      expect(client.space()).toBe(A_SPACE.id);
      expect(localStorage.getItem("rookery-space")).toBe(A_SPACE.id);
      expect(view.text()).toContain("Analysis");
    } finally {
      localStorage.removeItem("rookery-space");
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

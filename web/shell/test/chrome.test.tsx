// The chrome around the pages: notifications, the space switcher and the status.
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
  test("counts unread notifications and marks them read", async () => {
    const view = await mount(fakeClient({ user: STATIC_USER, notifications: [unread()] }));
    try {
      const bell = view.find("button[aria-label*='unread']");
      expect(bell?.getAttribute("aria-label")).toContain("1 unread");
      await view.clickElement(bell as HTMLElement);

      const markAll = view.findAll("button").find((button) => button.textContent?.includes("Mark all read"));
      expect(markAll).toBeDefined();
      await view.clickElement(markAll as HTMLElement);

      expect(view.find("button[aria-label='Notifications']")).not.toBeNull();
    } finally {
      view.unmount();
    }
  });

  test("switches the space every call is scoped to", async () => {
    const client = fakeClient({ user: STATIC_USER, spaces: [A_SPACE, ANOTHER_SPACE] });
    const view = await mount(client);
    try {
      expect(view.text()).toContain("All spaces");
      await view.click("button.nc-nav-item[aria-haspopup='menu']");

      const analysis = view.findAll(".nc-menu__item").find((item) => item.textContent?.includes("Analysis"));
      expect(analysis).toBeDefined();
      await view.clickElement(analysis as HTMLElement);

      expect(client.space()).toBe(A_SPACE.id);
      expect(localStorage.getItem("rookery-space")).toBe(A_SPACE.id);
      expect(view.text()).toContain("Analysis");
    } finally {
      localStorage.removeItem("rookery-space");
      view.unmount();
    }
  });

  test("says the link is up once the events stream answers", async () => {
    const view = await mount(
      fakeClient({
        user: STATIC_USER,
        events: [create(EventSchema, { name: "rookery/hosts.changed" })],
      }),
    );
    try {
      expect(view.find(".nc-shell__status")?.textContent).toContain("Connected");
    } finally {
      view.unmount();
    }
  });
});

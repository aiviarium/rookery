// The home page: what the server holds, once someone is signed in.
import { describe, expect, test } from "bun:test";
import type { Host, Job, Plugin } from "@rookery/proto";
import { HostType } from "@rookery/proto";

import { A_SPACE, STATIC_USER, fakeClient } from "./fakes.ts";
import { mount } from "./mount.tsx";

describe("the home page", () => {
  test("shows what is running and the person's hosts and spaces", async () => {
    const client = fakeClient({
      user: STATIC_USER,
      spaces: [A_SPACE],
      hosts: [{ name: "studio", type: HostType.MANAGED } as Host],
      plugins: [{ id: "example", name: "Example", version: "0.1.0", enabled: true } as Plugin],
      jobs: [{ id: "01hjob", kind: "host.apply", state: "running" } as Job],
    });
    const view = await mount(client);
    try {
      expect(view.text()).toContain("Ada");
      expect(view.find("a[href='/hosts/studio']")?.textContent).toContain("studio");
      expect(view.find(`a[href='/spaces/${A_SPACE.id}']`)?.textContent).toContain(A_SPACE.name);
      expect(view.text()).toContain("Running");
      // Nothing needs the person, so that card is not there.
      expect(view.text()).not.toContain("Needs you");
      // The sidebar's own navigation is there too.
      expect(view.find(".nc-nav a[href='/']")?.textContent).toContain("Home");
    } finally {
      view.unmount();
    }
  });

  test("points a server without hosts at the hosts page", async () => {
    const view = await mount(fakeClient({ user: STATIC_USER }));
    try {
      expect(view.text()).toContain("Add your first host");
      await view.button("Add host");
      expect(`${location.pathname}${location.search}`).toBe("/hosts?add=1");
    } finally {
      view.unmount();
    }
  });
});

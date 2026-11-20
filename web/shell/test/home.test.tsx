// The home page: what the server holds, once someone is signed in.
import { describe, expect, test } from "bun:test";
import type { Host, Job, Plugin } from "@rookery/proto";
import { HostType } from "@rookery/proto";

import { A_SPACE, STATIC_USER, fakeClient } from "./fakes.ts";
import { mount } from "./mount.tsx";

describe("the home page", () => {
  test("counts what the server holds", async () => {
    const client = fakeClient({
      user: STATIC_USER,
      spaces: [A_SPACE],
      hosts: [{ name: "studio", type: HostType.MANAGED } as Host],
      plugins: [{ id: "example", name: "Example", version: "0.1.0", enabled: true } as Plugin],
      jobs: [{ id: "01hjob", kind: "host.apply", state: "running" } as Job],
    });
    const view = await mount(client);
    try {
      expect(view.text()).toContain("Welcome, Ada Lovelace");
      expect(view.text()).toContain("studio");
      expect(view.text()).toContain("Example");
      // The sidebar's own navigation is there too.
      expect(view.find(".nc-nav a[href='/']")?.textContent).toContain("Home");
    } finally {
      view.unmount();
    }
  });

  test("points a server without hosts at the command that adds one", async () => {
    const view = await mount(fakeClient({ user: STATIC_USER }));
    try {
      expect(view.text()).toContain("No hosts yet");
      expect(view.text()).toContain("rookery host add");
    } finally {
      view.unmount();
    }
  });
});

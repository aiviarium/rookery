// A plugin's client half: its page in the navigation, and its mount in the outlet.
import { describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import type { LoadedClientHalf } from "@rookery/client";
import { PluginContributionSchema } from "@rookery/proto";

import { STATIC_USER, fakeClient } from "./fakes.ts";
import { mount } from "./mount.tsx";

/** notesHalf is a client half with one view and one navigation entry. */
function notesHalf(): LoadedClientHalf {
  return {
    plugin: "example",
    hash: "abc123",
    shell: false,
    contributes: {
      views: [{ id: "notes", label: "Notes", route: "/notes" }],
      nav: [{ view: "notes", group: "Workspace" }],
    },
    half: {
      views: {
        notes: (_context, element) => {
          element.textContent = "notes from the plugin";
        },
      },
    },
    url: "https://rookery.test/plugins/example/abc123/index.js",
  };
}

describe("a plugin's page", () => {
  test("appears in the navigation and draws into the outlet", async () => {
    const view = await mount(
      fakeClient(
        {
          user: STATIC_USER,
          contributions: [create(PluginContributionSchema, { plugin: "example", hash: "abc123", shell: false })],
        },
        [notesHalf()],
      ),
      { identityProvider: false },
      "/notes",
    );
    try {
      expect(view.findAll(".nc-section-heading").some((heading) => heading.textContent?.includes("Workspace"))).toBe(true);
      const link = view.find("a[href='/notes']");
      expect(link?.textContent).toContain("Notes");
      expect(view.text()).toContain("notes from the plugin");
    } finally {
      view.unmount();
    }
  });
});

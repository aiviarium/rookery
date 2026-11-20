import { describe, expect, test } from "bun:test";

import { createRookery } from "../src/transport.ts";
import { clientHalfUrl, clientContext, loadClientHalves } from "../src/loader.ts";
import type { PluginContributions } from "../src/contributions.ts";

/** contributions is what the edge says the installed plugins add. */
function contributions(): readonly PluginContributions[] {
  return [
    {
      plugin: "notes",
      hash: "abc123",
      contributes: {
        views: [{ id: "notes", label: "Notes", route: "/notes" }],
        nav: [{ view: "notes", group: "Workspace" }],
      },
    },
    { plugin: "quiet", hash: "", contributes: {} },
    { plugin: "broken", hash: "def456" },
    { plugin: "shell", hash: "fee1", shell: true, contributes: {} },
    { plugin: "off", hash: "0ff" },
  ];
}

/** client is a client that reaches a made-up server and makes no calls. */
function client(space = "") {
  return createRookery({
    baseUrl: "https://rookery.test",
    fetch: (async () => {
      throw new Error("the test made no HTTP call");
    }) as unknown as typeof fetch,
    autoSignIn: false,
    space,
  });
}

describe("loadClientHalves", () => {
  test("imports each client half from its immutable route", async () => {
    const imported: string[] = [];
    const mount = () => undefined;
    const halves = await loadClientHalves(client("01hspace"), {
      contributions: contributions(),
      enabled: (entry) => entry.plugin !== "off",
      importModule: async (url) => {
        imported.push(url);
        if (url.includes("/broken/")) throw new Error("the module is not there");
        return { default: { views: { notes: mount }, hostTabs: {} } };
      },
      onError: () => undefined,
    });

    expect(imported).toEqual([
      "https://rookery.test/plugins/notes/abc123/index.js",
      "https://rookery.test/plugins/broken/def456/index.js",
      "https://rookery.test/plugins/shell/fee1/index.js",
    ]);
    expect(halves.map((half) => half.plugin)).toEqual(["notes", "shell"]);
    expect(halves[0]?.contributes.views?.[0]?.route).toBe("/notes");
    expect(halves[1]?.shell).toBe(true);
    expect(halves[0]?.half.views?.["notes"]).toBe(mount);
  });

  test("reports a module that is not a client half", async () => {
    const errors: Error[] = [];
    const halves = await loadClientHalves(client(), {
      contributions: [{ plugin: "wrong", hash: "1" }],
      importModule: async () => ({ default: { views: "notes" } }),
      onError: (error) => errors.push(error),
    });
    expect(halves).toEqual([]);
    expect(errors[0]?.message).toContain("does not default-export a client half");
  });

  test("asks the edge when it was given no contributions", async () => {
    let asked = 0;
    const halves = await loadClientHalves(client(), {
      source: async () => {
        asked += 1;
        return [{ plugin: "a", hash: "1" }];
      },
      importModule: async () => ({ default: {} }),
    });
    expect(asked).toBe(1);
    expect(halves.map((half) => half.plugin)).toEqual(["a"]);
  });

  test("builds the context a half mounts with", () => {
    const rookery = client("01hspace");
    const context = clientContext(
      rookery,
      {
        plugin: "notes",
        hash: "abc",
        shell: false,
        contributes: { views: [{ id: "notes", label: "Notes", route: "/notes" }] },
        half: {},
        url: "https://rookery.test/plugins/notes/abc/index.js",
      },
      () => undefined,
    );
    expect(context.plugin.id).toBe("notes");
    expect(context.rookery.space()).toBe("01hspace");
    expect(context.contributes.views?.[0]?.label).toBe("Notes");
  });

  test("names a half's URL from its plugin and hash", () => {
    expect(clientHalfUrl(client(), { plugin: "a b", hash: "c/d" })).toBe(
      "https://rookery.test/plugins/a%20b/c%2Fd/index.js",
    );
  });
});

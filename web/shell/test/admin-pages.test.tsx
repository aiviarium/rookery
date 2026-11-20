// The server's own pages, one main interaction each: the thing a person does on
// the page, and what the page shows afterwards.
import { describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { TimestampSchema } from "@bufbuild/protobuf/wkt";
import { cbor } from "@rookery/client";
import {
  CollectionSchema,
  ConnectionTypeSchema,
  GroupSchema,
  KeySchema,
  KnownHostSchema,
  SettingSchema,
  SpaceKindSchema,
  UserSchema,
} from "@rookery/proto";
import type { FakeSite } from "./fakes.ts";
import { A_SPACE, fakeClient } from "./fakes.ts";
import { mount } from "./mount.tsx";

const AT = create(TimestampSchema, { seconds: 1_700_000_000n });

/** site is a signed-in server with the people the pages need. */
function site(extra: Partial<FakeSite> = {}): FakeSite {
  const user = create(UserSchema, { id: "01huser", email: "ada@example.test", name: "Ada Lovelace", admin: true });
  return {
    user,
    spaces: [A_SPACE],
    spaceKinds: [create(SpaceKindSchema, { id: "team", label: "Team" })],
    users: [user, create(UserSchema, { id: "01hother", email: "grace@example.test", name: "Grace Hopper" })],
    groups: [create(GroupSchema, { id: "01hgroup", name: "Operators", members: [user] })],
    ...extra,
  };
}

describe("the people page", () => {
  test("makes a sign-in link an admin can hand over", async () => {
    const made = site();
    const view = await mount(fakeClient(made), { identityProvider: false }, "/people");
    try {
      await view.type("#link-email", "grace@example.test");
      const button = view.findAll("button").find((candidate) => candidate.textContent?.includes("Make a link"));
      await view.clickElement(button as HTMLElement);
      expect(view.text()).toContain("/auth/link/link-token");
      expect(view.text()).toContain("grace%40example.test");
    } finally {
      view.unmount();
    }
  });
});

describe("the spaces page", () => {
  test("makes a space and opens it", async () => {
    const spaceSite = site();
    const view = await mount(fakeClient(spaceSite), { identityProvider: false }, "/spaces");
    try {
      await view.choose("[data-part='select'] button", "Team");
      await view.type("#space-name", "Simulations");
      const button = view.findAll("button").find((candidate) => candidate.textContent === "Create");
      await view.clickElement(button as HTMLElement);
      expect(view.text()).toContain("Simulations");
      expect(spaceSite.spaces?.some((space) => space.name === "Simulations")).toBe(true);
    } finally {
      view.unmount();
    }
  });
});

describe("the settings page", () => {
  test("saves a setting's value in the chosen scope", async () => {
    const settingsSite = site({
      settings: [
        create(SettingSchema, {
          key: "rookery/instance_name",
          type: "string",
          scopes: ["global", "user"],
          valueCbor: cbor.encode("Rookery"),
          scope: "default",
        }),
      ],
    });
    const view = await mount(fakeClient(settingsSite), { identityProvider: false }, "/settings");
    try {
      expect((view.find("#setting-rookery-instance_name") as HTMLInputElement).value).toBe("Rookery");
      await view.type("#setting-rookery-instance_name", "Hatchery");
      const save = view.findAll("button").find((candidate) => candidate.textContent === "Save");
      await view.clickElement(save as HTMLElement);
      expect(cbor.decode(settingsSite.settings?.[0]?.valueCbor)).toBe("Hatchery");
      expect(settingsSite.settings?.[0]?.scope).toBe("global");
    } finally {
      view.unmount();
    }
  });
});

describe("the secrets page", () => {
  test("sets a secret and lists its name, never its value", async () => {
    const secretSite = site();
    const view = await mount(fakeClient(secretSite), { identityProvider: false }, "/settings");
    try {
      await view.click("#settings-tabs-tab-secrets");
      await view.type("#secret-name", "github_token");
      await view.type("#secret-value", "hunter2-not-in-the-page");
      const set = view.findAll("button").find((candidate) => candidate.textContent === "Set");
      await view.clickElement(set as HTMLElement);
      expect(view.text()).toContain("github_token");
      expect(view.text()).not.toContain("hunter2-not-in-the-page");
      expect(secretSite.secrets?.[0]?.name).toBe("github_token");
      expect(secretSite.secrets?.[0]?.scope).toBe("global");
    } finally {
      view.unmount();
    }
  });
});

describe("the connections page", () => {
  test("creates a connection from a declared type", async () => {
    const connectionSite = site({
      connectionTypes: [
        create(ConnectionTypeSchema, {
          id: "registry",
          label: "Registry",
          fields: [{ $typeName: "rookery.v1.ConnectionField", name: "url", label: "URL", secret: false, required: true }],
        }),
      ],
    });
    const view = await mount(fakeClient(connectionSite), { identityProvider: false }, "/settings");
    try {
      await view.click("#settings-tabs-tab-connections");
      await view.choose("[data-part='select'] button", "Registry");
      await view.type("#connection-name", "lab-registry");
      await view.type("#connection-field-url", "https://registry.example.test");
      const createButton = view.findAll("button").find((candidate) => candidate.textContent === "Create");
      await view.clickElement(createButton as HTMLElement);
      expect(view.text()).toContain("lab-registry");
      expect(connectionSite.connections?.[0]?.name).toBe("lab-registry");
    } finally {
      view.unmount();
    }
  });
});

describe("the keys page", () => {
  test("rotates the key ring", async () => {
    const keySite = site({ keys: [create(KeySchema, { id: "01hkey", name: "default", type: "ssh-ed25519", fingerprint: "SHA256:old", active: true })] });
    const view = await mount(fakeClient(keySite), { identityProvider: false }, "/keys");
    try {
      await view.type("#key-name", "second");
      const rotate = view.findAll("button").find((candidate) => candidate.textContent === "Rotate");
      await view.clickElement(rotate as HTMLElement);
      expect(view.text()).toContain("SHA256:new1");
      expect(keySite.keys?.length).toBe(2);
    } finally {
      view.unmount();
    }
  });

  test("trusts a host key that changed", async () => {
    const keySite = site({
      knownHosts: [create(KnownHostSchema, { address: "192.0.2.4:22", host: "forge", fingerprint: "SHA256:old", pendingFingerprint: "SHA256:new" })],
    });
    const view = await mount(fakeClient(keySite), { identityProvider: false }, "/keys");
    try {
      const trust = view.findAll("button").find((candidate) => candidate.textContent?.includes("Trust the new key"));
      await view.clickElement(trust as HTMLElement);
      expect(keySite.knownHosts?.[0]?.fingerprint).toBe("SHA256:new");
      expect(keySite.knownHosts?.[0]?.pendingFingerprint).toBe("");
    } finally {
      view.unmount();
    }
  });
});

describe("the tailnet page", () => {
  test("configures the control server", async () => {
    const tailnetSite = site();
    const view = await mount(fakeClient(tailnetSite), { identityProvider: false }, "/tailnet");
    try {
      await view.type("#tailnet-api-key", "tskey-test");
      const save = view.findAll("button").find((candidate) => candidate.textContent === "Save");
      await view.clickElement(save as HTMLElement);
      expect(view.text()).toContain("configured");
      expect(view.text()).toContain("controlplane.tailscale.com");
    } finally {
      view.unmount();
    }
  });
});

describe("the plugins page", () => {
  test("disables an installed plugin", async () => {
    const pluginSite = site({
      plugins: [
        { $typeName: "rookery.v1.Plugin", id: "example", name: "Example", version: "0.1.0", enabled: true, contributesJson: new Uint8Array(), manifest: new Uint8Array() } as never,
      ],
    });
    const view = await mount(fakeClient(pluginSite), { identityProvider: false }, "/plugins");
    try {
      expect(view.find("a[href='/plugins']")?.textContent).toContain("Plugins");
      const disable = view.findAll("button").find((candidate) => candidate.textContent === "Disable");
      await view.clickElement(disable as HTMLElement);
      expect(pluginSite.plugins?.[0]?.enabled).toBe(false);
      expect(view.text()).toContain("disabled");
    } finally {
      view.unmount();
    }
  });
});

describe("the datastore page", () => {
  test("shows a collection's records and their history", async () => {
    const dataSite = site({
      collections: [create(CollectionSchema, { plugin: "example", name: "notes", scope: "space", fields: ["title"], history: true })],
      records: { "example/notes": [{ id: "01hnote", rev: 2, title: "A note" }] },
      history: { "example/notes/01hnote": [cbor.encode({ id: "01hnote", rev: 1, principal: "user:01huser", record: { title: "First" } })] },
    });
    const view = await mount(fakeClient(dataSite), { identityProvider: false }, "/data");
    try {
      await view.choose("[data-part='select'] button", "example/notes");
      expect(view.text()).toContain("A note");
      const row = view.findAll("tbody tr")[0];
      await view.clickElement(row as HTMLElement);
      expect(view.text()).toContain("First");
    } finally {
      view.unmount();
    }
  });
});

describe("the documents page", () => {
  test("creates a document and shows its id", async () => {
    const documentSite = site();
    const view = await mount(fakeClient(documentSite), { identityProvider: false }, "/documents");
    try {
      await view.type("#document-parent", "space://01hspace");
      const createButton = view.findAll("button").find((candidate) => candidate.textContent === "Create");
      await view.clickElement(createButton as HTMLElement);
      expect(documentSite.documents?.[0]?.type).toBe("yjs");
      expect(view.text()).toContain("01hdocument1");
    } finally {
      view.unmount();
    }
  });
});

describe("the server page", () => {
  test("shows the deployment's counts and its audit trail", async () => {
    const statusSite = site({
      audit: [
        {
          $typeName: "rookery.v1.AuditEntry",
          id: 1n,
          at: AT,
          principal: "user:01huser",
          procedure: "rookery.v1.SpaceService/Create",
          resource: "space://01hspace",
          outcome: "ok",
          detail: "",
        },
      ],
    });
    const original = globalThis.fetch;
    globalThis.fetch = (async () => new Response("ok", { status: 200 })) as unknown as typeof fetch;
    const view = await mount(fakeClient(statusSite), { identityProvider: false }, "/status");
    try {
      expect(view.text()).toContain("leader");
      expect(view.text()).toContain("SpaceService/Create");
    } finally {
      globalThis.fetch = original;
      view.unmount();
    }
  });
});

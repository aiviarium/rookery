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
    const view = await mount(fakeClient(site()), { identityProvider: false }, "/admin/people");
    try {
      await view.action("invite");
      await view.fill("Email", "grace@example.test");
      await view.submit();
      const shown = view.findAll("input").map((input) => (input as HTMLInputElement).value);
      expect(shown.some((value) => value.includes("/auth/link/link-token"))).toBe(true);
      expect(shown.some((value) => value.includes("grace%40example.test"))).toBe(true);
    } finally {
      view.unmount();
    }
  });
});

describe("the spaces page", () => {
  test("makes a space", async () => {
    const spaceSite = site();
    const view = await mount(fakeClient(spaceSite), { identityProvider: false }, "/admin/spaces");
    try {
      await view.action("new");
      // One kind of space needs no choice.
      expect(view.text()).not.toContain("Kind");
      await view.fill("Name", "Simulations");
      await view.submit();
      const made = spaceSite.spaces?.find((space) => space.name === "Simulations");
      expect(made).toBeDefined();
      // Making a space opens it.
      expect(location.pathname).toBe(`/spaces/${made?.id}`);
    } finally {
      view.unmount();
    }
  });
});

describe("the settings place", () => {
  const settingsSite = () =>
    site({
      settings: [
        create(SettingSchema, {
          key: "rookery/instance_name",
          title: "Name",
          description: "The name the shell shows.",
          category: "General",
          plugin: "rookery",
          type: "string",
          scopes: ["global"],
          editable: ["global"],
          defaultCbor: cbor.encode("Rookery"),
        }),
        create(SettingSchema, {
          key: "rookery/notifications",
          title: "Notifications",
          plugin: "rookery",
          type: "bool",
          scopes: ["global", "user"],
          editable: ["global", "user"],
          defaultCbor: cbor.encode(true),
          valuesCbor: { global: cbor.encode(false) },
        }),
      ],
    });

  test("says where a value comes from and sets it at the scope chosen", async () => {
    const made = settingsSite();
    const view = await mount(fakeClient(made), { identityProvider: false }, "/settings");
    try {
      const row = () => view.find("[data-key='rookery/notifications']")!;
      expect(row().textContent).toContain("Set for everyone");
      // What the person cannot set at the scope is not there.
      expect(view.find("[data-key='rookery/instance_name']")).toBeNull();

      await view.clickElement(row().querySelector("[role='switch']") as HTMLElement);
      expect(cbor.decode(made.settings?.[1]?.valuesCbor.user)).toBe(true);
      expect(row().hasAttribute("data-modified")).toBe(true);
      // The sidebar counts what is modified at the scope.
      expect(view.find(".shell-sidebar")?.textContent).toContain("1");

      await view.clickElement(row().querySelector("[data-part='setting-reset']") as HTMLElement);
      expect(made.settings?.[1]?.valuesCbor.user).toBeUndefined();
    } finally {
      view.unmount();
    }
  });

  test("edits the server's values at the Server scope", async () => {
    const made = settingsSite();
    const client = fakeClient(made);
    const calls: string[] = [];
    const original = client.settings.set.bind(client.settings);
    (client.settings as { set: unknown }).set = (request: { key: string; scope: string }) => {
      calls.push(`${request.key}@${request.scope}`);
      return original(request as never);
    };
    const view = await mount(client, { identityProvider: false }, "/settings?scope=global");
    try {
      const input = view.find("[data-key='rookery/instance_name'] input:not([aria-hidden])") as HTMLInputElement;
      expect(input.value).toBe("Rookery");
      input.value = "Hatchery";
      input.dispatchEvent(new InputEvent("input", { bubbles: true }));
      input.dispatchEvent(new FocusEvent("blur"));
      await view.click("[data-key='rookery/instance_name']");
      expect(calls).toEqual(["rookery/instance_name@global"]);
      expect(cbor.decode(made.settings?.[0]?.valuesCbor.global)).toBe("Hatchery");
    } finally {
      view.unmount();
    }
  });
});

describe("the secrets page", () => {
  test("sets a secret and lists its name, never its value", async () => {
    const secretSite = site();
    const view = await mount(fakeClient(secretSite), { identityProvider: false }, "/admin/secrets");
    try {
      await view.action("add");
      await view.fill("Name", "github_token");
      await view.fill("Value", "hunter2-not-in-the-page");
      await view.submit();
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
    const view = await mount(fakeClient(connectionSite), { identityProvider: false }, "/admin/connections");
    try {
      await view.action("add");
      await view.select("Type", "Registry");
      await view.fill("Name", "lab-registry");
      await view.fill("URL", "https://registry.example.test");
      await view.submit();
      expect(connectionSite.connections?.[0]?.name).toBe("lab-registry");
      expect(view.text()).toContain("lab-registry");
    } finally {
      view.unmount();
    }
  });
});

describe("the keys page", () => {
  test("rotates the key ring", async () => {
    const keySite = site({ keys: [create(KeySchema, { id: "01hkey", name: "default", type: "ssh-ed25519", fingerprint: "SHA256:old", active: true })] });
    const view = await mount(fakeClient(keySite), { identityProvider: false }, "/admin/keys");
    try {
      await view.action("rotate");
      await view.fill("Name", "second");
      await view.submit();
      expect(keySite.keys?.length).toBe(2);
      expect(view.text()).toContain("SHA256:new1");
    } finally {
      view.unmount();
    }
  });

  test("trusts a host key that changed", async () => {
    const keySite = site({
      knownHosts: [create(KnownHostSchema, { address: "192.0.2.4:22", host: "forge", fingerprint: "SHA256:old", pendingFingerprint: "SHA256:new" })],
    });
    const view = await mount(fakeClient(keySite), { identityProvider: false }, "/admin/keys");
    try {
      await view.rowMenu("192.0.2.4:22", "Trust the new key");
      expect(keySite.knownHosts?.[0]?.fingerprint).toBe("SHA256:new");
      expect(keySite.knownHosts?.[0]?.pendingFingerprint).toBe("");
    } finally {
      view.unmount();
    }
  });
});

describe("the tailnet page", () => {
  test("configures the control server", async () => {
    const view = await mount(fakeClient(site()), { identityProvider: false }, "/admin/tailnet");
    try {
      // An unconfigured tailnet offers one thing: configuring it.
      await view.button("Configure");
      await view.fill("API key", "tskey-test");
      await view.submit();
      expect(view.text()).toContain("controlplane.tailscale.com");
    } finally {
      view.unmount();
    }
  });
});

describe("the plugins page", () => {
  test("turns an installed plugin off", async () => {
    const pluginSite = site({
      plugins: [
        { $typeName: "rookery.v1.Plugin", id: "example", name: "Example", version: "0.1.0", enabled: true, contributesJson: new Uint8Array(), manifest: new Uint8Array() } as never,
      ],
    });
    const view = await mount(fakeClient(pluginSite), { identityProvider: false }, "/admin/plugins");
    try {
      await view.rowMenu("example", "Turn off");
      expect(pluginSite.plugins?.[0]?.enabled).toBe(false);
      expect(view.text()).toContain("off");
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
    const view = await mount(fakeClient(dataSite), { identityProvider: false }, "/admin/data");
    try {
      const collection = view.findAll("[data-part='list-row']").find((row) => row.textContent?.includes("example/notes"));
      await view.clickElement(collection?.querySelector<HTMLElement>("button, a, [role='button']") ?? (collection as HTMLElement));
      expect(view.text()).toContain("A note");
      const row = view.findAll("tbody tr")[0];
      await view.clickElement(row as HTMLElement);
      // A DOM without layout has no room for the panel beside the table, so it opens over it.
      if (view.find("[data-part='view-aside-toggle']") !== null) await view.click("[data-part='view-aside-toggle']");
      expect(view.text()).toContain("First");
    } finally {
      view.unmount();
    }
  });
});

describe("the documents page", () => {
  test("creates a document and shows its id", async () => {
    const documentSite = site();
    const view = await mount(fakeClient(documentSite), { identityProvider: false }, "/admin/documents");
    try {
      await view.action("new");
      await view.fill("Parent", "space://01hspace");
      await view.submit();
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
    const view = await mount(fakeClient(statusSite), { identityProvider: false }, "/admin/server");
    try {
      expect(view.text()).toContain("Leads");
      expect(view.text()).toContain("SpaceService/Create");
    } finally {
      globalThis.fetch = original;
      view.unmount();
    }
  });
});

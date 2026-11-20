// The infrastructure pages: the hosts and their page, a job's log, the machines a
// provisioner made, and the terminals a host opens.
import { describe, expect, test } from "bun:test";
import type {
  Action,
  Host,
  Job,
  JobLine,
  ProvisionerEntry,
  ProvisionerResource,
  ProvisionerType,
} from "@rookery/proto";
import { HostType } from "@rookery/proto";

import { STATIC_USER, fakeClient } from "./fakes.ts";
import { mount } from "./mount.tsx";

/** studio is a managed host whose profile is applied and current. */
const STUDIO = {
  name: "studio",
  type: HostType.MANAGED,
  profile: { type: HostType.MANAGED, address: "192.0.2.10", port: 22, sshUser: "ops", operator: "ops", tailnet: true },
  address: "100.64.0.10",
  appliedDigest: "sha256:aaa",
  digest: "sha256:aaa",
  labels: { role: "desk" },
} as unknown as Host;

/** forge is a host Rookery has never applied. */
const FORGE = {
  name: "forge",
  type: HostType.SSH,
  profile: { type: HostType.SSH, address: "192.0.2.20", port: 22, sshUser: "ops" },
  labels: {},
} as unknown as Host;

describe("the hosts page", () => {
  test("shows each host's state and labels, and opens its page", async () => {
    const view = await mount(
      fakeClient({
        user: STATIC_USER,
        hosts: [STUDIO, FORGE],
        actions: [{ name: "update", label: "Update packages", group: "host", user: 2 } as unknown as Action],
      }),
      undefined,
      "/hosts",
    );
    try {
      expect(view.find("[data-part='data-table']")).not.toBeNull();
      expect(view.text()).toContain("studio");
      expect(view.text()).toContain("Current");
      expect(view.text()).toContain("Never applied");
      expect(view.text()).toContain("role=desk");

      await view.click("[data-row-id='studio']");
      expect(view.find("[data-part='view-header']")?.textContent).toContain("studio");
      expect(view.text()).toContain("Profile");
      expect(view.text()).toContain("ops@192.0.2.10");
      // The actions tab lists what the host's type and features declare.
      await view.click("#host-tabs-tab-actions");
      expect(view.text()).toContain("Update packages");
    } finally {
      view.unmount();
    }
  });

  test("adds a host from the dialog and opens its page", async () => {
    const site = { user: STATIC_USER, hosts: [] as Host[] };
    const view = await mount(fakeClient(site), undefined, "/hosts");
    try {
      await view.click("button[aria-label='Add host']");
      expect(view.find("[data-part='dialog']")).not.toBeNull();

      await view.type("input[placeholder='studio']", "builder");
      await view.type("input[placeholder='192.0.2.10']", "192.0.2.30");
      await view.click("[data-part='add-host-submit']");

      expect(site.hosts.map((host) => host.name)).toEqual(["builder"]);
      expect(view.find("[data-part='view-header']")?.textContent).toContain("builder");
    } finally {
      view.unmount();
    }
  });
});

describe("a job's page", () => {
  test("follows the log to the job's end", async () => {
    const job = { id: "01hjob", kind: "host.apply", state: "running", resource: "host://studio" } as unknown as Job;
    const lines: JobLine[] = [
      { seq: 1n, line: "Installing rookeryd" },
      { seq: 2n, line: "Joining the tailnet" },
    ] as JobLine[];
    const view = await mount(
      fakeClient({
        user: STATIC_USER,
        jobs: [job],
        logs: lines,
        jobEnd: { ...job, state: "succeeded" } as Job,
      }),
      undefined,
      "/jobs/01hjob",
    );
    try {
      expect(view.find("[data-part='log-viewer']")).not.toBeNull();
      expect(view.text()).toContain("Job log");
      expect(view.text()).toContain("Installing rookeryd");
      expect(view.text()).toContain("Joining the tailnet");
      expect(view.text()).toContain("Succeeded");
    } finally {
      view.unmount();
    }
  });
});

describe("the machines page", () => {
  test("lists a provisioner, its health and the machine it made", async () => {
    const type = {
      id: "incus/container",
      label: "Incus container",
      kinds: [{ id: "machine", label: "Machine", machine: true }],
      sizes: [{ id: "small", label: "Small", cpus: 2, memoryMb: 2048, diskGb: 20 }],
      actions: [{ name: "images", label: "List images" }],
    } as unknown as ProvisionerType;
    const entry = { name: "lab-incus", type: type.id, node: "studio" } as unknown as ProvisionerEntry;
    const machine = { id: "01hmachine", kind: "machine", name: "desk-3", provisioner: entry.name, address: "192.0.2.31" } as unknown as ProvisionerResource;
    const view = await mount(
      fakeClient({
        user: STATIC_USER,
        hosts: [STUDIO],
        types: [type],
        provisioners: [entry],
        machines: [machine],
      }),
      undefined,
      "/machines",
    );
    try {
      expect(view.text()).toContain("lab-incus");
      expect(view.text()).toContain("incus/container");
      expect(view.text()).toContain("Up");
      expect(view.text()).toContain("desk-3");
      expect(view.text()).toContain("192.0.2.31");
    } finally {
      view.unmount();
    }
  });

  test("makes a machine through the edge's own call", async () => {
    const type = {
      id: "example/fake",
      label: "Fake machine",
      kinds: [{ id: "machine", label: "Machine", machine: true }],
      actions: [],
    } as unknown as ProvisionerType;
    const entry = { name: "lab", type: type.id, node: "studio" } as unknown as ProvisionerEntry;
    const calls: string[] = [];
    const job = { id: "01hcreate", kind: "host.create", state: "queued", resource: "host://desk-4" } as unknown as Job;
    const view = await mount(
      fakeClient({ user: STATIC_USER, types: [type], provisioners: [entry], jobs: [job], calls }),
      undefined,
      "/machines",
    );
    try {
      const button = view.findAll("button").find(one => one.textContent?.trim() === "New machine");
      expect(button).toBeDefined();
      await view.clickElement(button as HTMLElement);
      await view.type("input[placeholder='desk-3']", "desk-4");
      const create = view.findAll("button").find(one => one.textContent?.trim() === "Create machine");
      expect(create).toBeDefined();
      await view.clickElement(create as HTMLElement);
      expect(calls).toContain("create desk-4");
    } finally {
      view.unmount();
    }
  });

  test("removes a machine through the edge's own call", async () => {
    const type = {
      id: "example/fake",
      label: "Fake machine",
      kinds: [{ id: "machine", label: "Machine", machine: true }],
      actions: [],
    } as unknown as ProvisionerType;
    const entry = { name: "lab", type: type.id } as unknown as ProvisionerEntry;
    const machine = { id: "01hmachine", kind: "machine", name: "desk-3", provisioner: entry.name } as unknown as ProvisionerResource;
    const calls: string[] = [];
    const job = { id: "01hremove", kind: "host.remove", state: "queued", resource: "host://desk-3" } as unknown as Job;
    const view = await mount(
      fakeClient({ user: STATIC_USER, types: [type], provisioners: [entry], machines: [machine], jobs: [job], calls }),
      undefined,
      "/machines",
    );
    try {
      await view.click("[data-row-id='01hmachine'] button");
      expect(calls).toContain("remove desk-3");
    } finally {
      view.unmount();
    }
  });
});

describe("the terminals page", () => {
  test("offers the first host's shell", async () => {
    const view = await mount(fakeClient({ user: STATIC_USER, hosts: [STUDIO] }), undefined, "/terminals");
    try {
      expect(view.text()).toContain("A new terminal");
      expect(view.text()).toContain("No terminal is open");
      // The host the page opens on is the first one the server lists.
      expect(view.find("[data-part='select'] button")?.textContent).toContain("studio");
    } finally {
      view.unmount();
    }
  });
});

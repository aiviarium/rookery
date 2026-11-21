// The fleet: the hosts and their page, the machines provisioners make as hosts,
// a job's log, and the terminals a host opens.
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
      // A host that is current says nothing about it; one never applied says so.
      const studio = view.find("tr[data-row-id='studio']");
      expect(studio?.textContent).toContain("role=desk");
      expect(studio?.textContent).not.toContain("Current");
      expect(view.find("tr[data-row-id='forge']")?.textContent).toContain("Not set up");

      await view.click("tr[data-row-id='studio']");
      expect(view.find("[data-part='view-header']")?.textContent).toContain("studio");
      // Inside a host the sidebar holds its places.
      expect(view.find(".shell-sidebar a[href='/hosts/studio/profile']")?.textContent).toContain("Profile");
      // A DOM without layout has no room for the details beside the overview, so they open over it.
      await view.click("[data-part='view-aside-toggle']");
      expect(view.text()).toContain("ops@192.0.2.10");
      // The overview lists what the host's type and features declare.
      expect(view.text()).toContain("Update packages");
    } finally {
      view.unmount();
    }
  });

  test("adds a host from the dialog and opens its page", async () => {
    const site = { user: STATIC_USER, hosts: [] as Host[] };
    const view = await mount(fakeClient(site), undefined, "/hosts");
    try {
      await view.action("add");
      expect(view.find("[data-part='dialog']")).not.toBeNull();

      await view.fill("Name", "builder");
      await view.fill("Address", "192.0.2.30");
      await view.submit();

      expect(site.hosts.map((host) => host.name)).toEqual(["builder"]);
      expect(view.find("[data-part='view-header']")?.textContent).toContain("builder");
    } finally {
      view.unmount();
    }
  });
});

describe("a host's overview", () => {
  test("runs an action from its play button and follows the job under the host", async () => {
    const job = { id: "01hjob", kind: "host.action", state: "running", resource: "host://studio", argsCbor: new Uint8Array() } as unknown as Job;
    const view = await mount(
      fakeClient({
        user: STATIC_USER,
        hosts: [STUDIO, FORGE],
        jobs: [job],
        actions: [{ name: "update", label: "Update packages", group: "", user: 2, arguments: [] } as unknown as Action],
      }),
      undefined,
      "/hosts/studio",
    );
    try {
      // The sidebar says where it came from, and the host is one of a kind in the path and the sidebar.
      expect(view.find("[data-part='sidebar-path']")?.textContent).toContain("Fleet");
      expect(view.find("[data-part='sidebar-switcher']")?.textContent).toContain("studio");
      expect(view.find("[data-part='view-header'] [data-part='crumb-switcher']")?.textContent).toContain("studio");
      // The row's play button runs the action; there is no second Run button.
      const row = view.find("[data-host-action='update']");
      expect([...(row?.querySelectorAll("button") ?? [])].some((button) => button.textContent?.trim() === "Run")).toBe(false);
      await view.click("[data-host-action='update'] [data-part='run-action']");
      expect(location.pathname).toBe("/hosts/studio/jobs/01hjob");
      expect(view.find(".shell-sidebar a[href='/hosts/studio/jobs']")?.getAttribute("aria-current")).toBe("page");
      expect(view.find("[data-part='view-header']")?.textContent).toContain("Jobs");
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
      expect(view.find("[data-part='log-viewer']")?.getAttribute("aria-label") ?? "Job log").toContain("Job log");
      expect(view.text()).toContain("Installing rookeryd");
      expect(view.text()).toContain("Joining the tailnet");
      expect(view.text()).toContain("Succeeded");
    } finally {
      view.unmount();
    }
  });
});

describe("machines and provisioners on the hosts page", () => {
  test("lists a machine as a host, and its provisioner in the next tab", async () => {
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
      "/hosts",
    );
    try {
      // The machine is a row of the hosts, being created until its host is there.
      const row = view.find("tr[data-row-id='desk-3']");
      expect(row?.textContent).toContain("192.0.2.31");
      expect(row?.textContent).toContain("lab-incus");
      expect(row?.textContent).toContain("Creating");
      await view.click("[role='tab'][id$='-tab-provisioners']");
      const provisioner = view.find("[data-provisioner='lab-incus']");
      expect(provisioner?.textContent).toContain("Incus container");
      // A healthy provisioner shows no state.
      expect(provisioner?.textContent).not.toContain("Up");
    } finally {
      view.unmount();
    }
  });

  test("makes a machine from Add host, asking only what a machine needs", async () => {
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
      "/hosts",
    );
    try {
      await view.action("add");
      await view.select("Machine", "New on lab");
      // The provisioner knows its engine: no address, user or port.
      expect(view.find("[data-part='dialog']")?.textContent).not.toContain("Address");
      await view.fill("Name", "desk-4");
      await view.submit();
      expect(calls).toContain("create desk-4");
    } finally {
      view.unmount();
    }
  });

  test("removes a machine from its row's menu", async () => {
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
      "/hosts",
    );
    try {
      await view.rowMenu("desk-3", "Remove machine");
      await view.submit();
      expect(calls).toContain("remove desk-3");
    } finally {
      view.unmount();
    }
  });
});

describe("the terminals page", () => {
  test("opens a terminal on the first host", async () => {
    const view = await mount(fakeClient({ user: STATIC_USER, hosts: [STUDIO] }), undefined, "/terminals");
    try {
      expect(view.text()).toContain("No terminals");
      await view.action("open");
      // The host the dialog offers first is the first one the server lists.
      const host = [...document.querySelectorAll<HTMLElement>("[data-part='select']")].find(
        (select) => select.querySelector(".nc-field__label")?.textContent === "Host",
      );
      expect(host?.querySelector("button")?.textContent).toContain("studio");
    } finally {
      view.unmount();
    }
  });
});

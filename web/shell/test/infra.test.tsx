// The infrastructure pages: the hosts and their page, a job's log, and the
// machines a provisioner made.
import { describe, expect, test } from "bun:test";
import type { Action, Host, Job, JobLine } from "@rookery/proto";
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
      expect(view.text()).toContain("studio");
      expect(view.text()).toContain("Current");
      expect(view.text()).toContain("Never applied");
      expect(view.text()).toContain("role=desk");

      await view.click("tr[data-row='studio']");
      expect(view.text()).toContain("Profile");
      expect(view.text()).toContain("ops@192.0.2.10");
      // The actions tab lists what the host's type and features declare.
      await view.click("[data-tab='actions']");
      expect(view.text()).toContain("Update packages");
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
      expect(view.text()).toContain("Job log");
      expect(view.text()).toContain("Installing rookeryd");
      expect(view.text()).toContain("Joining the tailnet");
      expect(view.text()).toContain("Succeeded");
    } finally {
      view.unmount();
    }
  });
});

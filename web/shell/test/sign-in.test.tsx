// Signing in: the ways into this deployment, before there is a session.
import { describe, expect, test } from "bun:test";

import { mount } from "./mount.tsx";
import { fakeClient } from "./fakes.ts";

describe("the sign-in page", () => {
  test("offers the identity provider when the deployment has one", async () => {
    const view = await mount(fakeClient({}), { identityProvider: true });
    try {
      expect(view.text()).toContain("Sign in with your identity provider");
      const link = view.find("a[href^='/auth/oidc/start']");
      expect(link).not.toBeNull();
      expect(link?.getAttribute("href")).toContain("next=%2F");
    } finally {
      view.unmount();
    }
  });

  test("says to ask an admin when there is no identity provider", async () => {
    const view = await mount(fakeClient({}));
    try {
      expect(view.text()).toContain("Ask an admin");
      expect(view.find("a[href^='/auth/oidc/start']")).toBeNull();
    } finally {
      view.unmount();
    }
  });
});

import { describe, expect, test } from "bun:test";

import { Code, ConnectError } from "@connectrpc/connect";

import { create } from "@bufbuild/protobuf";
import { UserSchema } from "@rookery/proto";

import { createSignIn } from "../src/session.ts";

/** ada is a signed-in user. */
function ada() {
  return create(UserSchema, { id: "01h", email: "ada@example.test", name: "Ada" });
}

describe("sign-in state", () => {
  test("starts unknown and turns signed-in on Me", async () => {
    const signIn = createSignIn({
      me: async () => ada(),
      signOut: async () => undefined,
    });
    expect(signIn.get().status).toBe("unknown");

    const seen: string[] = [];
    signIn.subscribe((state) => seen.push(state.status));
    const state = await signIn.refresh();
    expect(state.status).toBe("signed-in");
    expect(seen).toEqual(["unknown", "signed-in"]);
  });

  test("turns signed-out on an unauthenticated call", async () => {
    const signIn = createSignIn({
      me: async () => {
        throw new ConnectError("no session", Code.Unauthenticated);
      },
      signOut: async () => undefined,
    });
    expect((await signIn.refresh()).status).toBe("signed-out");
  });

  test("keeps another failure as an error, with its cause", async () => {
    const signIn = createSignIn({
      me: async () => {
        throw new ConnectError("standby", Code.Unavailable);
      },
      signOut: async () => undefined,
    });
    const state = await signIn.refresh();
    expect(state.status).toBe("error");
    if (state.status !== "error") throw new Error("the test expected an error");
    expect(state.error.message).toContain("standby");
  });

  test("signs out through the edge", async () => {
    let called = 0;
    const signIn = createSignIn({
      me: async () => ada(),
      signOut: async () => {
        called += 1;
      },
    });
    await signIn.refresh();
    await signIn.signOut();
    expect(called).toBe(1);
    expect(signIn.get().status).toBe("signed-out");
  });
});

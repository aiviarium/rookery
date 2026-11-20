import { describe, expect, test } from "bun:test";

import { create, toBinary } from "@bufbuild/protobuf";
import {
  AuthServiceMeResponseSchema,
  UserSchema,
} from "@rookery/proto";

import { createRookery } from "../src/transport.ts";

/** recordingFetch answers every call from answer, and records what was asked. */
function recordingFetch(
  answer: (request: Request) => Response,
): { fetch: typeof fetch; requests: Request[] } {
  const requests: Request[] = [];
  return {
    requests,
    fetch: (async (input: RequestInfo | URL, init?: RequestInit) => {
      const request = new Request(input as RequestInfo, init);
      requests.push(request);
      return answer(request);
    }) as typeof fetch,
  };
}

/** me is AuthService.Me's answer, in the binary the edge speaks by default. */
function me(email = "ada@example.test"): Response {
  const body = toBinary(
    AuthServiceMeResponseSchema,
    create(AuthServiceMeResponseSchema, {
      user: create(UserSchema, {
        id: "01huser",
        email,
        name: "Ada",
        admin: false,
        disabled: false,
      }),
    }),
  );
  return new Response(body, {
    status: 200,
    headers: {
      "Content-Type": "application/proto",
      "Connect-Protocol-Version": "1",
    },
  });
}

describe("createRookery", () => {
  test("signs in from the cookie and returns the user", async () => {
    const { fetch, requests } = recordingFetch(() => me());
    const rookery = createRookery({
      baseUrl: "https://rookery.test",
      fetch,
      csrf: () => "the-token",
      autoSignIn: false,
    });

    const state = await rookery.signIn.refresh();
    expect(state.status).toBe("signed-in");
    if (state.status !== "signed-in") throw new Error("the test expected a user");
    expect(state.user.email).toBe("ada@example.test");

    const request = requests[0];
    if (request === undefined) throw new Error("no call was made");
    expect(request.url).toBe("https://rookery.test/rookery.v1.AuthService/Me");
    expect(request.method).toBe("POST");
    expect(request.headers.get("Rookery-CSRF")).toBe("the-token");
    expect(request.headers.has("Rookery-Space")).toBe(false);
  });

  test("sends the current space on every call, and in a route's URL", async () => {
    const { fetch, requests } = recordingFetch(() => me());
    const rookery = createRookery({
      baseUrl: "https://rookery.test",
      fetch,
      csrf: () => "the-token",
      autoSignIn: false,
      space: "01hspace",
    });

    await rookery.signIn.refresh();
    expect(requests[0]?.headers.get("Rookery-Space")).toBe("01hspace");
    expect(rookery.webSocketUrl("/ws/documents/01hdoc")).toBe(
      "wss://rookery.test/ws/documents/01hdoc?space=01hspace",
    );

    rookery.setSpace("01hother");
    await rookery.signIn.refresh();
    expect(requests[1]?.headers.get("Rookery-Space")).toBe("01hother");
  });

  test("turns an unauthenticated Me into a signed-out state", async () => {
    const { fetch } = recordingFetch(
      () =>
        new Response(JSON.stringify({ code: "unauthenticated", message: "not signed in" }), {
          status: 401,
          headers: { "Content-Type": "application/json", "Connect-Protocol-Version": "1" },
        }),
    );
    const rookery = createRookery({
      baseUrl: "https://rookery.test",
      fetch,
      autoSignIn: false,
    });

    const state = await rookery.signIn.refresh();
    expect(state.status).toBe("signed-out");
  });

  test("signs out at the edge and clears the state", async () => {
    const { fetch, requests } = recordingFetch((request) =>
      request.url.endsWith("/auth/logout") ? new Response(null, { status: 204 }) : me(),
    );
    const rookery = createRookery({
      baseUrl: "https://rookery.test",
      fetch,
      csrf: () => "the-token",
      autoSignIn: false,
    });
    await rookery.signIn.refresh();

    await rookery.signIn.signOut();
    expect(rookery.signIn.get().status).toBe("signed-out");
    const logout = requests.find((request) => request.url.endsWith("/auth/logout"));
    if (logout === undefined) throw new Error("sign-out did not reach the edge");
    expect(logout.method).toBe("POST");
    expect(logout.headers.get("Rookery-CSRF")).toBe("the-token");
  });

  test("has a client for every service", () => {
    const { fetch } = recordingFetch(() => me());
    const rookery = createRookery({ baseUrl: "https://rookery.test", fetch, autoSignIn: false });
    expect(typeof rookery.auth.me).toBe("function");
    expect(typeof rookery.users.list).toBe("function");
    expect(typeof rookery.groups.list).toBe("function");
    expect(typeof rookery.access.check).toBe("function");
    expect(typeof rookery.spaces.list).toBe("function");
  });
});

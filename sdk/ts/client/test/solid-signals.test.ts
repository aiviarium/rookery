import { describe, expect, test } from "bun:test";

import { createRoot } from "solid-js";

import { watchable } from "../src/watchable.ts";
import { createQuery, fromWatchable, useSignIn } from "../src/solid/signals.ts";

/** root runs a body with an owner, and returns what it drew. */
function root<T>(body: (dispose: () => void) => T): T {
  return createRoot((dispose) => body(dispose));
}

/** tick lets a promise's answer land. */
async function tick(): Promise<void> {
  await Bun.sleep(5);
}

describe("fromWatchable", () => {
  test("reads a value that changes outside Solid", () => {
    const source = watchable(1);
    root(() => {
      const value = fromWatchable(source);
      expect(value()).toBe(1);
      source.set(2);
      expect(value()).toBe(2);
      source.update((previous) => previous + 1);
      expect(value()).toBe(3);
    });
  });

  test("stops listening when its owner is disposed", () => {
    const source = watchable(0);
    let value = () => 0;
    root((dispose) => {
      value = fromWatchable(source);
      dispose();
    });
    source.set(5);
    expect(value()).toBe(0);
  });
});

describe("createQuery", () => {
  test("answers a call, and answers again on refresh", async () => {
    let calls = 0;
    let query = createQuery(async () => {
      calls += 1;
      return calls;
    });
    root(() => {
      query = createQuery(async () => {
        calls += 1;
        return calls;
      });
      expect(query.status()).toBe("loading");
      expect(query()).toBeUndefined();
      expect(query.pending()).toBe(true);
    });

    await tick();
    expect(query.status()).toBe("ready");
    expect(query()).toBe(2);
    expect(query.pending()).toBe(false);

    const answer = await query.refresh();
    expect(answer).toBe(3);
    expect(query()).toBe(3);
    query.dispose();
  });

  test("keeps the old answer while a refresh is in flight", async () => {
    let release = (): void => undefined;
    let calls = 0;
    const query = createQuery(async () => {
      calls += 1;
      if (calls > 1) await new Promise<void>((done) => (release = done));
      return calls;
    });
    root(() => query);
    await tick();
    expect(query()).toBe(1);

    const refreshed = query.refresh();
    expect(query.pending()).toBe(true);
    expect(query()).toBe(1);
    release();
    await refreshed;
    expect(query()).toBe(2);
    expect(query.pending()).toBe(false);
    query.dispose();
  });

  test("keeps a failure as the query's error", async () => {
    const query = createQuery(async () => {
      throw new Error("the edge is a standby");
    });
    root(() => query);
    await tick();
    expect(query.status()).toBe("error");
    expect(query.error()?.message).toBe("the edge is a standby");
    expect(query()).toBeUndefined();
    query.dispose();
  });

  test("drops an answer that lands after disposal", async () => {
    let release = (): void => undefined;
    const query = createQuery(
      () => new Promise<string>((done) => (release = () => done("late"))),
    );
    root(() => query);
    query.dispose();
    release();
    await tick();
    expect(query()).toBeUndefined();
    expect(query.status()).toBe("loading");
  });
});

describe("useSignIn", () => {
  test("reads the client's state, and asks the edge once", async () => {
    let asked = 0;
    const state = watchable<{ status: "unknown" } | { status: "signed-out" }>({ status: "unknown" });
    const signIn = {
      get: state.get,
      set: state.set,
      update: state.update,
      subscribe: state.subscribe,
      refresh: async () => {
        asked += 1;
        state.set({ status: "signed-out" });
        return state.get();
      },
      signOut: async () => undefined,
    };
    const accessor = root(() => useSignIn({ signIn }));
    await tick();
    expect(accessor().status).toBe("signed-out");
    expect(accessor.user()).toBeUndefined();
    expect(asked).toBe(1);
  });
});

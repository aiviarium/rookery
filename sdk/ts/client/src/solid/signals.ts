// Solid 2 bindings for the client's own state and its calls.
//
// The client's sources push: a socket reports a status, the sign-in state changes
// after a call, a live query's stream delivers an event. Each is read through a
// signal this package keeps up to date, because the value is known the moment it
// arrives and a shell draws the waiting state from the value it has.
//
// A call is a query: it holds the answer, says whether one is in flight, and asks
// again on `refresh()` — after a write, or when the space changes.
import { type Accessor, createSignal, getOwner, onCleanup } from "solid-js";
import type { User } from "@rookery/proto";

import type { Rookery } from "../transport.ts";
import type { SignIn, SignInSnapshot } from "../session.ts";
import { asError } from "../session.ts";
import type { Watchable } from "../watchable.ts";

/**
 * fromWatchable reads a value that changes outside Solid — the sign-in state, a
 * socket's status — as a signal. Its subscription ends with the owner.
 */
export function fromWatchable<T>(source: Watchable<T>): Accessor<T> {
  const [value, setValue] = createSignal<T>(source.get() as Exclude<T, Function>);
  const stop = source.subscribe(setValue);
  if (getOwner() !== null) onCleanup(stop);
  return value;
}

/** QueryStatus is where a call is. */
export type QueryStatus = "loading" | "ready" | "error";

/**
 * Query is a call's answer: read it for the answer the last call gave, `status()`
 * for where it is, and `refresh()` to ask again. A refresh keeps the old answer
 * visible while the new one is in flight, which `pending()` reports.
 */
export interface Query<T> {
  /** read is the last answer, or undefined before the first one. */
  (): T | undefined;
  /** status is loading, ready or error. */
  status: Accessor<QueryStatus>;
  /** error is why the last call failed, while status is error. */
  error: Accessor<Error | undefined>;
  /** pending is true while a call is in flight, including a refresh. */
  pending: Accessor<boolean>;
  /** refresh asks again and resolves with the answer, or rejects with its failure. */
  refresh(): Promise<T>;
  /** dispose drops an answer that lands after the component is gone. */
  dispose(): void;
}

/**
 * createQuery runs a call once, and again on every `refresh()`. A call that lands
 * after a newer one, or after disposal, is dropped.
 */
export function createQuery<T>(run: () => Promise<T>): Query<T> {
  const [value, setValue] = createSignal<T>();
  const [status, setStatus] = createSignal<QueryStatus>("loading");
  const [error, setError] = createSignal<Error>();
  const [pending, setPending] = createSignal(false);

  let generation = 0;
  let disposed = false;

  const start = (): Promise<T> => {
    const mine = ++generation;
    const current = (): boolean => !disposed && mine === generation;
    setPending(true);
    return run().then(
      (answer) => {
        if (current()) {
          setValue(() => answer);
          setStatus("ready");
          setError(undefined);
        }
        return answer;
      },
      (failure: unknown) => {
        if (current()) {
          setStatus("error");
          setError(asError(failure));
        }
        throw failure;
      },
    ).finally(() => {
      if (current()) setPending(false);
    });
  };

  const query = (() => value()) as Query<T>;
  query.status = status;
  query.error = error;
  query.pending = pending;
  query.refresh = () => start();
  query.dispose = () => {
    disposed = true;
  };

  void start().catch(() => undefined);
  if (getOwner() !== null) onCleanup(() => query.dispose());
  return query;
}

/** SignInAccessor is who is signed in, as a signal, and the calls that change it. */
export interface SignInAccessor extends Accessor<SignInSnapshot> {
  /** user is the signed-in user, or undefined. */
  user: Accessor<User | undefined>;
  /** refresh asks the edge who the caller is. */
  refresh(): Promise<SignInSnapshot>;
  /** signOut ends the session. */
  signOut(): Promise<void>;
}

/**
 * useSignIn reads the client's sign-in state, and asks the edge the first time
 * unless the client already knows.
 */
export function useSignIn(rookery: Pick<Rookery, "signIn">): SignInAccessor {
  const signIn: SignIn = rookery.signIn;
  const state = fromWatchable(signIn);
  const accessor = state as SignInAccessor;
  accessor.user = () => {
    const snapshot = state();
    return snapshot.status === "signed-in" ? snapshot.user : undefined;
  };
  accessor.refresh = () => signIn.refresh();
  accessor.signOut = () => signIn.signOut();
  if (signIn.get().status === "unknown") {
    // The state is the client's, not this component's, so one ask serves a page.
    void signIn.refresh().catch(() => undefined);
  }
  return accessor;
}

/**
 * onDispose runs a close with the owner, for what a helper opened. It is
 * `onCleanup` under the name this package uses.
 */
export function onDispose(close: () => void): void {
  if (getOwner() !== null) onCleanup(close);
}

// A call a person starts: a button's work, a dialog's submit.
//
// It keeps whether the call is running and why it failed, so a button shows its
// spinner and a dialog its error without each page keeping two signals.
import { type Accessor, createSignal } from "solid-js";
import { toast } from "@aiviarium/nocturne";

import { errorMessage } from "./errors.ts";

/** Action is a call that can run once at a time. */
export interface Action<A extends unknown[]> {
  /** run starts the call unless it is running, and resolves true when it succeeded. */
  run(...args: A): Promise<boolean>;
  /** busy is true while it runs. */
  readonly busy: Accessor<boolean>;
  /** error is why the last run failed, until the next one starts. */
  readonly error: Accessor<string | undefined>;
  /** reset forgets the error. */
  reset(): void;
}

/** ActionOptions say what a run tells the person. */
export interface ActionOptions {
  /** done is a toast's title after a run succeeds. */
  readonly done?: string;
  /** failed is a toast's title after a run fails; without it the error is only kept. */
  readonly failed?: string;
}

/** createAction wraps a call for a button or a dialog. */
export function createAction<A extends unknown[]>(
  call: (...args: A) => Promise<unknown>,
  options: ActionOptions = {},
): Action<A> {
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string>();
  return {
    busy,
    error,
    reset: () => setError(undefined),
    async run(...args) {
      if (busy()) return false;
      setBusy(true);
      setError(undefined);
      try {
        await call(...args);
        if (options.done !== undefined) toast({ title: options.done, tone: "success" });
        return true;
      } catch (failure) {
        const message = errorMessage(failure);
        setError(message);
        if (options.failed !== undefined) toast({ title: options.failed, description: message, tone: "danger" });
        return false;
      } finally {
        setBusy(false);
      }
    },
  };
}

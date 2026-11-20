// Who the caller is. The edge keeps a device session in an HttpOnly cookie, so a
// page never holds a secret: it asks `AuthService.Me`, and the browser sends the
// cookie.
import { Code, ConnectError } from "@connectrpc/connect";
import type { User } from "@rookery/proto";

import { type Watchable, watchable } from "./watchable.ts";

/** SignInSnapshot is the sign-in state a page renders from. */
export type SignInSnapshot =
  | { readonly status: "unknown" }
  | { readonly status: "signed-in"; readonly user: User }
  | { readonly status: "signed-out" }
  | { readonly status: "error"; readonly error: Error };

/** AuthConfig is what a sign-in page needs before anyone has signed in. */
export interface AuthConfig {
  /** identityProvider is whether the deployment signs browsers in through OIDC. */
  readonly identityProvider: boolean;
}

/**
 * fetchAuthConfig asks the edge which ways in this deployment has. It is public:
 * the sign-in page needs it before there is a session.
 */
export async function fetchAuthConfig(
  baseUrl: string,
  options: { fetch?: typeof fetch } = {},
): Promise<AuthConfig> {
  const response = await (options.fetch ?? globalThis.fetch)(`${baseUrl}/auth/config`, {
    method: "GET",
    credentials: "include",
    headers: { Accept: "application/json" },
  });
  if (!response.ok) {
    throw new Error(`reading the sign-in configuration: ${response.status} ${response.statusText}`);
  }
  return (await response.json()) as AuthConfig;
}

/** SignIn is the sign-in state, and the calls that change it. */
export interface SignIn extends Watchable<SignInSnapshot> {
  /** refresh asks AuthService.Me and returns the state it found. */
  refresh(): Promise<SignInSnapshot>;
  /** signOut ends the session at the edge and clears the state. */
  signOut(): Promise<void>;
}

/** SignInCalls is what the sign-in state needs from the edge. */
export interface SignInCalls {
  /** me is AuthService.Me's user. */
  me(): Promise<User>;
  /** signOut posts to the edge's sign-out route. */
  signOut(): Promise<void>;
}

/** createSignIn makes the sign-in state over the edge's calls. */
export function createSignIn(calls: SignInCalls): SignIn {
  const state = watchable<SignInSnapshot>({ status: "unknown" });
  return {
    get: () => state.get(),
    set: state.set,
    update: state.update,
    subscribe: state.subscribe,
    async refresh() {
      try {
        const user = await calls.me();
        state.set({ status: "signed-in", user });
      } catch (error) {
        if (isUnauthenticated(error)) {
          state.set({ status: "signed-out" });
        } else {
          state.set({ status: "error", error: asError(error) });
        }
      }
      return state.get();
    },
    async signOut() {
      await calls.signOut();
      state.set({ status: "signed-out" });
    },
  };
}

/** isUnauthenticated reports whether a call failed because nobody is signed in. */
export function isUnauthenticated(error: unknown): boolean {
  return error instanceof ConnectError && error.code === Code.Unauthenticated;
}

/** isPermissionDenied reports whether a call was refused for lack of access. */
export function isPermissionDenied(error: unknown): boolean {
  return error instanceof ConnectError && error.code === Code.PermissionDenied;
}

/** asError is anything thrown, as an Error. */
export function asError(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error));
}

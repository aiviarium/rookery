// Device-session auth and sign-in flow helpers for Rookery's HTTP auth endpoints:
// GET /auth/session, GET /auth/login, GET /auth/link/{code}, POST /auth/logout.
import { type Accessor, createSignal, getOwner, onCleanup } from "solid-js"
import type { ErrorBody, Me, SignedOut, User } from "./generated"
import type { Connection } from "./ws"

export type SignInOptions = Pick<SignedOut, "devLogin" | "sso" | "local">

export type SessionCheckResult =
  | { authenticated: true; user: User }
  | { authenticated: false; options: SignInOptions; error?: ErrorBody; unreachable?: boolean }

export interface AuthFetchOptions {
  /** Base origin (e.g. `http://127.0.0.1:4790`). Defaults to relative URLs on the current origin. */
  baseUrl?: string
  /** Fetch implementation (defaults to `globalThis.fetch`). */
  fetch?: typeof fetch
}

function stripTrailingSlashes(s: string): string {
  let end = s.length
  while (end > 0 && s.charCodeAt(end - 1) === 47) end--
  return end === s.length ? s : s.slice(0, end)
}

function joinUrl(baseUrl: string | undefined, path: string): string {
  if (!baseUrl) return path
  return `${stripTrailingSlashes(baseUrl)}${path}`
}

/** Queries `GET /auth/session` to see whether a device session is active and which sign-in methods exist. */
export async function fetchSession(opts: AuthFetchOptions = {}): Promise<SessionCheckResult> {
  const f = opts.fetch ?? fetch
  const fallback: SignInOptions = { devLogin: false, local: false, sso: null }
  let res: Response
  try {
    res = await f(joinUrl(opts.baseUrl, "/auth/session"), {
      headers: { accept: "application/json" },
    })
  } catch {
    return { authenticated: false, options: fallback, unreachable: true }
  }
  if (res.ok) {
    const body = (await res.json()) as { user: User }
    return { authenticated: true, user: body.user }
  }
  if (res.status === 401) {
    const body = (await res.json().catch(() => null)) as Partial<SignedOut> | null
    return {
      authenticated: false,
      options: {
        devLogin: Boolean(body?.devLogin),
        local: Boolean(body?.local),
        sso: body?.sso ?? null,
      },
      error: body?.error,
    }
  }
  return { authenticated: false, options: fallback, unreachable: true }
}

/** URL that initiates OIDC sign-in and redirects back to `returnTo` on the same origin. */
export function loginUrl(
  returnTo: string = typeof location !== "undefined" ? location.pathname + location.search : "/",
  baseUrl?: string,
): string {
  return joinUrl(baseUrl, `/auth/login?return_to=${encodeURIComponent(returnTo)}`)
}

/** URL for development sign-in when the server offers `devLogin`. */
export function devLoginUrl(
  user: string,
  returnTo: string = typeof location !== "undefined" ? location.pathname + location.search : "/",
  baseUrl?: string,
): string {
  return joinUrl(baseUrl, `/dev/login?user=${encodeURIComponent(user)}&next=${encodeURIComponent(returnTo)}`)
}

/** URL for a one-time local-account sign-in link (`GET /auth/link/<code>`). */
export function linkConfirmUrl(code: string, baseUrl?: string): string {
  return joinUrl(baseUrl, `/auth/link/${encodeURIComponent(code)}`)
}

const LINK_CODE_RE = /^[A-Za-z0-9_-]{4,256}$/

/**
 * Normalizes a pasted local sign-in link (full URL, `/auth/link/<code>` path, or bare code)
 * into a safe same-origin `/auth/link/<code>` path, or returns `null` if invalid or cross-origin.
 */
export function parseSignInLink(input: string, expectedOrigin?: string): string | null {
  const trimmed = input.trim()
  if (!trimmed) return null
  if (LINK_CODE_RE.test(trimmed)) return `/auth/link/${encodeURIComponent(trimmed)}`
  if (trimmed.startsWith("/auth/link/")) {
    const code = trimmed.slice("/auth/link/".length).split(/[?#]/)[0] ?? ""
    return LINK_CODE_RE.test(code) ? `/auth/link/${encodeURIComponent(code)}` : null
  }
  try {
    const url = new URL(trimmed)
    if (expectedOrigin && url.origin !== expectedOrigin) return null
    if (!url.pathname.startsWith("/auth/link/")) return null
    const code = url.pathname.slice("/auth/link/".length)
    return LINK_CODE_RE.test(code) ? `/auth/link/${encodeURIComponent(code)}` : null
  } catch {
    return null
  }
}

export interface SignOutOptions extends AuthFetchOptions {
  /** Connection to stop before logging out. */
  conn?: Connection
  /** Optional hook called before sending `/auth/logout` (e.g. clearing unsent drafts). */
  onBeforeSignOut?: () => void
}

/** Ends the current device session via `POST /auth/logout` and returns any IdP end-session redirect URL. */
export async function signOut(opts: SignOutOptions = {}): Promise<{ redirect: string | null }> {
  opts.conn?.stop()
  opts.onBeforeSignOut?.()
  const f = opts.fetch ?? fetch
  try {
    const res = await f(joinUrl(opts.baseUrl, "/auth/logout"), {
      method: "POST",
      headers: { accept: "application/json" },
    })
    if (res.ok) {
      const body = (await res.json().catch(() => null)) as { redirect?: string | null } | null
      return { redirect: body?.redirect ?? null }
    }
  } catch {
    // Offline: caller still transitions to signed-out UI.
  }
  return { redirect: null }
}

export interface SessionStore {
  me: Accessor<Me | null>
  setMe: (m: Me | null) => void
  signedOut: Accessor<boolean>
  setSignedOut: (v: boolean) => void
  refresh(): Promise<Me | null>
  dispose(): void
}

/** Tracks `auth.me` and `signedOut` reactively on a `Connection`, refreshing `auth.me` on every open. */
export function createSession(conn: Connection): SessionStore {
  const [me, setMe] = createSignal<Me | null>(null)
  const [signedOut, setSignedOut] = createSignal(false)

  const refresh = async (): Promise<Me | null> => {
    try {
      const info = await conn.call<Me>("auth.me")
      setMe(info)
      return info
    } catch {
      return null
    }
  }

  const offOpen = conn.onOpen(() => {
    void refresh()
  })

  const dispose = () => {
    offOpen()
  }
  if (getOwner()) onCleanup(dispose)

  return { me, setMe, signedOut, setSignedOut, refresh, dispose }
}

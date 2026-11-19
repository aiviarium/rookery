import { afterEach, describe, expect, test } from "bun:test"
import { createRoot } from "solid-js"
import {
  createSession,
  devLoginUrl,
  fetchSession,
  linkConfirmUrl,
  loginUrl,
  parseSignInLink,
  signOut,
} from "../src/auth"
import { Connection } from "../src/ws"

const cleanup: Array<() => void> = []
afterEach(() => {
  while (cleanup.length) cleanup.pop()!()
})

async function waitFor(fn: () => boolean, timeoutMs = 3000): Promise<void> {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (fn()) return
    await Bun.sleep(10)
  }
  throw new Error("timed out waiting for condition")
}

describe("@rookery/client auth helpers", () => {
  test("fetchSession handles 200 signed-in, 401 signed-out options, and unreachable", async () => {
    let mode: "ok" | "signed-out" | "error" = "signed-out"
    const server = Bun.serve({
      port: 0,
      fetch(req) {
        const url = new URL(req.url)
        if (url.pathname === "/auth/session") {
          if (mode === "ok") {
            return Response.json({
              user: { id: "u-ada", name: "Ada", email: "ada@example.test", roles: ["admin"] },
            })
          }
          if (mode === "signed-out") {
            return Response.json(
              {
                error: { code: "UNAUTHENTICATED", message: "sign in required" },
                devLogin: false,
                local: true,
                sso: { name: "Example SSO", ready: true },
              },
              { status: 401 },
            )
          }
          return new Response("down", { status: 503 })
        }
        if (url.pathname === "/auth/logout" && req.method === "POST") {
          return Response.json({ redirect: "https://idp.example.test/logout" })
        }
        return new Response("not found", { status: 404 })
      },
    })
    cleanup.push(() => server.stop(true))
    const baseUrl = `http://127.0.0.1:${server.port}`

    const signedOut = await fetchSession({ baseUrl })
    expect(signedOut.authenticated).toBe(false)
    if (!signedOut.authenticated) {
      expect(signedOut.options).toEqual({
        devLogin: false,
        local: true,
        sso: { name: "Example SSO", ready: true },
      })
      expect(signedOut.error?.code).toBe("UNAUTHENTICATED")
    }

    mode = "ok"
    const signedIn = await fetchSession({ baseUrl })
    expect(signedIn.authenticated).toBe(true)
    if (signedIn.authenticated) {
      expect(signedIn.user.id).toBe("u-ada")
    }

    mode = "error"
    const unreachable = await fetchSession({ baseUrl })
    expect(unreachable.authenticated).toBe(false)
    if (!unreachable.authenticated) {
      expect(unreachable.unreachable).toBe(true)
    }

    let beforeCalled = false
    const out = await signOut({
      baseUrl,
      onBeforeSignOut: () => {
        beforeCalled = true
      },
    })
    expect(beforeCalled).toBe(true)
    expect(out.redirect).toBe("https://idp.example.test/logout")
  })

  test("URL builders and parseSignInLink validate and normalize links", () => {
    expect(loginUrl("/fleet?tab=1")).toBe("/auth/login?return_to=%2Ffleet%3Ftab%3D1")
    expect(devLoginUrl("alice", "/home")).toBe("/dev/login?user=alice&next=%2Fhome")
    expect(linkConfirmUrl("code_1234")).toBe("/auth/link/code_1234")

    expect(parseSignInLink("abc_1234-XYZ")).toBe("/auth/link/abc_1234-XYZ")
    expect(parseSignInLink("/auth/link/abc_1234?foo=1")).toBe("/auth/link/abc_1234")
    expect(
      parseSignInLink("https://rookery.example.test/auth/link/tok_9999", "https://rookery.example.test"),
    ).toBe("/auth/link/tok_9999")
    expect(
      parseSignInLink("https://evil.example.test/auth/link/tok_9999", "https://rookery.example.test"),
    ).toBeNull()
    expect(parseSignInLink("https://rookery.example.test/not-a-link")).toBeNull()
    expect(parseSignInLink("   ")).toBeNull()
    expect(parseSignInLink("x")).toBeNull()
  })

  test("createSession refreshes auth.me when the WebSocket opens", async () => {
    const server = Bun.serve({
      port: 0,
      fetch(req, srv) {
        if (new URL(req.url).pathname === "/ws" && srv.upgrade(req)) return undefined
        return new Response("not found", { status: 404 })
      },
      websocket: {
        message(ws, data) {
          const msg = JSON.parse(String(data))
          if (msg.method === "auth.me") {
            ws.send(
              JSON.stringify({
                jsonrpc: "2.0",
                id: msg.id,
                result: {
                  user: { id: "u-dev", name: "Dev User", roles: ["admin"] },
                  principal: { kind: "user", id: "u-dev" },
                  dev: true,
                  localAuth: true,
                  kernelVersion: "0.1.0",
                  apiVersion: 1,
                },
              }),
            )
          }
        },
      },
    })
    cleanup.push(() => server.stop(true))

    const conn = new Connection({
      url: `ws://127.0.0.1:${server.port}/ws`,
      checkSession: async () => true,
      reconnect: false,
    })
    cleanup.push(() => conn.stop())

    let store!: ReturnType<typeof createSession>
    let disposeRoot: (() => void) | undefined
    createRoot((dispose) => {
      disposeRoot = dispose
      store = createSession(conn)
    })
    cleanup.push(() => disposeRoot?.())

    conn.connect()
    await waitFor(() => store.me() !== null)
    expect(store.me()?.user?.name).toBe("Dev User")
    expect(store.signedOut()).toBe(false)
  })
})

// Tests the plugin client-half loader and SDK contract against a bare SolidJS fixture page
// (no UI library, no Nocturne) and an in-process fake Rookery server serving content-hashed bundles.
import { afterEach, describe, expect, test } from "bun:test"
import { createSignal, For, Show } from "solid-js"
import { render } from "solid-js/web"
import type { PluginInfo } from "../src/generated"
import { createPluginLoader, publishSharedModules, SHARED_MODULE_GLOBAL } from "../src/loader"
import { Connection } from "../src/ws"

const cleanup: Array<() => void> = []
afterEach(() => {
  while (cleanup.length) cleanup.pop()!()
  document.body.innerHTML = ""
  for (const el of Array.from(document.head.querySelectorAll("link[data-plugin]"))) {
    el.remove()
  }
})

async function waitFor(fn: () => boolean, timeoutMs = 3000): Promise<void> {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (fn()) return
    await Bun.sleep(10)
  }
  throw new Error("timed out waiting for condition")
}

// Simulates the client-half bundle output produced by Rookery's plugin bundler:
// reads shared modules (`@rookery/sdk/client`, `solid-js/web`) from `globalThis.__rookery.modules`.
function makePluginBundle(versionTag: string): string {
  return `
    const mods = globalThis[${JSON.stringify(SHARED_MODULE_GLOBAL)}].modules;
    const { defineClient } = mods["@rookery/sdk/client"];
    export default defineClient((rookery) => {
      let count = 0;
      rookery.views.register("hello.main", (props) => {
        props.view.setTitle("Hello " + ${JSON.stringify(versionTag)});
        const el = document.createElement("section");
        el.setAttribute("data-hello-version", ${JSON.stringify(versionTag)});
        el.textContent = "Hello " + ${JSON.stringify(versionTag)} + " param=" + (props.view.params.id ?? "none");
        return el;
      });
      rookery.commands.register("hello.bump", {
        title: "Bump Hello",
        run: async () => {
          count += 1;
          const pong = await rookery.rpc.call("hello.ping", { count });
          return pong;
        },
      });
      rookery.nav.section("hello.section", {
        items: () => [{ id: "hello.main", title: "Main " + ${JSON.stringify(versionTag)}, view: "hello.main" }],
      });
      rookery.nav.badge("hello.main", () => ${JSON.stringify(versionTag)});
      rookery.workbench.statusItem("hello.status", () => {
        const span = document.createElement("span");
        span.setAttribute("data-status", ${JSON.stringify(versionTag)});
        span.textContent = "status:" + ${JSON.stringify(versionTag)};
        return span;
      });
    });
  `
}

function makeBrokenPluginBundle(): string {
  return `
    const mods = globalThis[${JSON.stringify(SHARED_MODULE_GLOBAL)}].modules;
    const { defineClient } = mods["@rookery/sdk/client"];
    export default defineClient(() => {
      throw new Error("broken plugin boom");
    });
  `
}

function helloPluginInfo(hash: string): PluginInfo {
  return {
    id: "hello",
    state: "active",
    client: {
      script: `/plugins/hello/client.${hash}.js`,
      styles: `/plugins/hello/client.${hash}.css`,
      hash,
    },
    manifest: {
      id: "hello",
      name: "Hello",
      version: "0.1.0",
      contributes: {
        views: [{ id: "hello.main", title: "Hello Main" }],
        routes: [{ path: ":id", view: "hello.main" }],
        commands: [{ id: "hello.bump", title: "Bump Hello" }],
        collections: [],
        settings: [],
        adminPages: [
          { id: "hello.admin", title: "Hello Admin", view: "hello.main", section: "system", order: 10 },
        ],
        nav: [{ id: "hello.main", label: "Hello", view: "hello.main" }],
      },
    },
  } as unknown as PluginInfo
}

function brokenPluginInfo(hash: string): PluginInfo {
  return {
    id: "broken",
    state: "active",
    client: {
      script: `/plugins/broken/client.${hash}.js`,
      styles: null,
      hash,
    },
    manifest: {
      id: "broken",
      name: "Broken",
      version: "0.1.0",
      contributes: {
        views: [],
        routes: [],
        commands: [],
        collections: [],
        settings: [],
        adminPages: [],
        nav: [],
      },
    },
  } as unknown as PluginInfo
}

describe("@rookery/client plugin loader & bare fixture page", () => {
  test("loads content-hashed client half, mounts view in bare SolidJS fixture, hot-reloads, isolates faults, and unloads", async () => {
    const sockets = new Set<any>()
    const assetRequests: string[] = []
    let plugins: PluginInfo[] = [helloPluginInfo("hashv1"), brokenPluginInfo("brokenv1")]

    const bundles: Record<string, { body: string; contentType: string }> = {
      "/plugins/hello/client.hashv1.js": {
        body: makePluginBundle("v1"),
        contentType: "application/javascript",
      },
      "/plugins/hello/client.hashv1.css": { body: ".hello { color: green; }", contentType: "text/css" },
      "/plugins/hello/client.hashv2.js": {
        body: makePluginBundle("v2"),
        contentType: "application/javascript",
      },
      "/plugins/hello/client.hashv2.css": { body: ".hello { color: blue; }", contentType: "text/css" },
      "/plugins/broken/client.brokenv1.js": {
        body: makeBrokenPluginBundle(),
        contentType: "application/javascript",
      },
    }

    const server = Bun.serve({
      port: 0,
      fetch(req, srv) {
        const url = new URL(req.url)
        if (url.pathname === "/ws" && srv.upgrade(req)) return undefined
        const asset = bundles[url.pathname]
        if (asset) {
          assetRequests.push(url.pathname)
          return new Response(asset.body, { headers: { "content-type": asset.contentType } })
        }
        return new Response("not found", { status: 404 })
      },
      websocket: {
        open(ws) {
          sockets.add(ws)
        },
        close(ws) {
          sockets.delete(ws)
        },
        message(ws, data) {
          const msg = JSON.parse(String(data))
          if (msg.method === "plugins.list") {
            ws.send(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: { plugins } }))
          } else if (msg.method === "commands.list") {
            ws.send(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: { commands: [] } }))
          } else if (msg.method === "events.subscribe" || msg.method === "events.unsubscribe") {
            ws.send(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: {} }))
          } else if (msg.method === "hello.ping") {
            ws.send(
              JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: { ok: true, count: msg.params.count } }),
            )
          }
        },
      },
    })
    cleanup.push(() => server.stop(true))
    const baseUrl = `http://127.0.0.1:${server.port}`

    const notify = (method: string, params: unknown) => {
      const msg = JSON.stringify({ jsonrpc: "2.0", method, params })
      for (const ws of sockets) ws.send(msg)
    }

    publishSharedModules()

    const conn = new Connection({
      url: `ws://127.0.0.1:${server.port}/ws`,
      checkSession: async () => true,
      reconnect: false,
    })
    cleanup.push(() => conn.stop())

    // Import HTTP ESM bundles via fetch -> data URL so Bun executes the exact served bundle bytes.
    const loader = createPluginLoader(conn, {
      baseUrl,
      importModule: async (url) => {
        const res = await fetch(url)
        if (!res.ok) throw new Error(`HTTP ${res.status} for ${url}`)
        const src = await res.text()
        const dataUrl = `data:text/javascript;base64,${Buffer.from(src).toString("base64")}`
        return import(dataUrl)
      },
    })
    cleanup.push(() => loader.stop())

    const [title, setTitle] = createSignal("")
    const [activeView] = createSignal("hello.main")

    // Bare fixture page: zero UI components, plain HTML elements exercising the framework contract.
    const host = document.createElement("div")
    document.body.appendChild(host)
    const unmount = render(() => {
      const entry = () => loader.views()[activeView()]
      return (
        <div data-fixture="bare">
          <header data-title>{title()}</header>
          <nav data-nav>
            <For each={Object.values(loader.navSections())}>
              {(s) => (
                <For each={s.section.items?.() ?? []}>
                  {(item) => (
                    <span data-nav-item={item.id}>
                      {item.title}:{loader.navBadges()[item.id]?.() ?? ""}
                    </span>
                  )}
                </For>
              )}
            </For>
          </nav>
          <main data-main>
            <Show when={entry()} keyed>
              {(v) => {
                const Comp = v.component
                return (
                  <Comp
                    view={{
                      id: activeView(),
                      plugin: v.plugin,
                      path: "42",
                      params: { id: "42" },
                      setTitle,
                      setDirty: () => {},
                      setLoading: () => {},
                    }}
                  />
                )
              }}
            </Show>
          </main>
          <footer data-status-bar>
            <For each={Object.values(loader.statusItems())}>{(s) => <s.component />}</For>
          </footer>
        </div>
      )
    }, host)
    cleanup.push(unmount)

    loader.start()
    conn.connect()
    await loader.clientsSettled(3000)

    // Healthy plugin loaded & rendered, broken plugin isolated as failed.
    expect(loader.clients.hello?.status).toBe("ready")
    expect(loader.clients.hello?.hash).toBe("hashv1")
    expect(loader.clients.broken?.status).toBe("failed")
    expect(loader.clients.broken?.error).toContain("broken plugin boom")

    expect(assetRequests).toContain("/plugins/hello/client.hashv1.js")
    expect(host.querySelector("[data-hello-version='v1']")?.textContent).toBe("Hello v1 param=42")
    expect(host.querySelector("[data-title]")?.textContent).toBe("Hello v1")
    expect(host.querySelector("[data-nav-item='hello.main']")?.textContent).toBe("Main v1:v1")
    expect(host.querySelector("[data-status='v1']")?.textContent).toBe("status:v1")
    expect(loader.adminPages().map((p) => p.page.view)).toEqual(["hello.main"])

    const styleLink = document.head.querySelector("link[data-plugin='hello']") as HTMLLinkElement | null
    expect(styleLink?.getAttribute("href")).toBe(`${baseUrl}/plugins/hello/client.hashv1.css`)

    // Command execution invokes plugin RPC over /ws.
    const cmdRes = await loader.runCommand("hello.bump")
    expect(cmdRes).toEqual({ ok: true, count: 1 })

    // Hot reload with new content hash v2.
    plugins = [helloPluginInfo("hashv2")]
    notify("plugins.reloaded", { id: "hello", hash: "hashv2" })
    await waitFor(() => loader.clients.hello?.hash === "hashv2" && loader.clients.hello?.status === "ready")

    expect(assetRequests).toContain("/plugins/hello/client.hashv2.js")
    expect(host.querySelector("[data-hello-version='v2']")?.textContent).toBe("Hello v2 param=42")
    expect(host.querySelector("[data-title]")?.textContent).toBe("Hello v2")
    expect(host.querySelector("[data-nav-item='hello.main']")?.textContent).toBe("Main v2:v2")
    expect(host.querySelector("[data-status='v2']")?.textContent).toBe("status:v2")
    const reloadedStyle = document.head.querySelector("link[data-plugin='hello']") as HTMLLinkElement | null
    expect(reloadedStyle?.getAttribute("href")).toBe(`${baseUrl}/plugins/hello/client.hashv2.css`)

    // Unload plugin via plugins.changed -> everything cleaned up.
    plugins = []
    notify("plugins.changed", { id: "hello", state: "unloaded" })
    await waitFor(() => !loader.views()["hello.main"])
    expect(host.querySelector("[data-hello-version]")).toBeNull()
    expect(document.head.querySelector("link[data-plugin='hello']")).toBeNull()
    expect(Object.keys(loader.statusItems())).toHaveLength(0)
  })
})

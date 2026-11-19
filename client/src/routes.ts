// Pure URL matching and construction for plugin `contributes.routes` and shell built-in routes.
import type { PluginInfo, RouteContribution } from "./generated"

export interface RouteEntry {
  view: string
  plugin: string
  /** Leading-slash pattern such as `/fleet/hosts/:name` or `/settings/*rest`. */
  pattern: string
  segments: string[]
  title?: string
}

export interface RouteMatch {
  view: string
  plugin: string
  /** Positional param values joined by `/`, matching `ViewHandle.path`. */
  path: string
  /** Named route parameters merged with URL query parameters. */
  params: Record<string, string>
}

function splitPath(path: string): string[] {
  return path
    .replace(/^\/+|\/+$/g, "")
    .split("/")
    .filter(Boolean)
}

function decodeSegment(s: string): string {
  try {
    return decodeURIComponent(s)
  } catch {
    return s
  }
}

/** Builds a route table from shell built-ins and loaded plugins' `contributes.routes`. */
export function buildRouteTable(
  plugins: Iterable<PluginInfo>,
  builtins: Array<{ view: string; pattern: string; title?: string }> = [],
): RouteEntry[] {
  const entries: RouteEntry[] = builtins.map((b) => ({
    view: b.view,
    plugin: "shell",
    pattern: b.pattern.startsWith("/") ? b.pattern : `/${b.pattern}`,
    segments: splitPath(b.pattern),
    title: b.title,
  }))

  for (const p of plugins) {
    const manifest = p.manifest
    if (!manifest || p.state === "invalid") continue
    const ns = manifest.namespace ?? manifest.id
    for (const r of manifest.contributes.routes as RouteContribution[]) {
      const rel = r.path.replace(/^\/+/, "")
      const full = rel ? `/${ns}/${rel}` : `/${ns}`
      entries.push({
        view: r.view,
        plugin: p.id,
        pattern: full,
        segments: splitPath(full),
      })
    }
  }

  // More specific routes (more static segments, fewer wildcards) match first.
  entries.sort((a, b) => score(b.segments) - score(a.segments))
  return entries
}

function score(segments: string[]): number {
  let s = 0
  for (const seg of segments) {
    if (seg.startsWith("*")) s += 1
    else if (seg.startsWith(":")) s += 10
    else s += 100
  }
  return s
}

/** Matches a pathname and optional query string against a route table. */
export function matchUrl(table: RouteEntry[], pathname: string, search = ""): RouteMatch | null {
  const parts = splitPath(pathname)
  const query: Record<string, string> = {}
  if (search) {
    for (const [k, v] of new URLSearchParams(search)) query[k] = v
  }
  for (const entry of table) {
    const params: Record<string, string> = { ...query }
    const positional: string[] = []
    let matched = true
    let i = 0
    for (; i < entry.segments.length; i++) {
      const seg = entry.segments[i]!
      if (seg.startsWith("*")) {
        const name = seg.slice(1) || "rest"
        const rest = parts.slice(i).map(decodeSegment).join("/")
        params[name] = rest
        if (rest) positional.push(rest)
        i = parts.length
        break
      }
      if (i >= parts.length) {
        matched = false
        break
      }
      const actual = decodeSegment(parts[i]!)
      if (seg.startsWith(":")) {
        params[seg.slice(1)] = actual
        positional.push(actual)
      } else if (seg !== actual) {
        matched = false
        break
      }
    }
    if (matched && i === parts.length) {
      return {
        view: entry.view,
        plugin: entry.plugin,
        path: positional.join("/"),
        params,
      }
    }
  }
  return null
}

/** Builds a URL for a view from the route table, filling positional `path` or named `params`. */
export function viewUrl(
  table: RouteEntry[],
  view: string,
  opts: { path?: string; params?: Record<string, string> } = {},
): string | null {
  const entry = table.find((e) => e.view === view)
  if (!entry) return null
  const positional = opts.path ? splitPath(opts.path) : []
  let posIdx = 0
  const used = new Set<string>()
  const out: string[] = []
  for (const seg of entry.segments) {
    if (seg.startsWith("*")) {
      const name = seg.slice(1) || "rest"
      const val = opts.params?.[name] ?? positional.slice(posIdx).join("/")
      used.add(name)
      if (val) {
        for (const p of splitPath(val)) out.push(encodeURIComponent(p))
      }
    } else if (seg.startsWith(":")) {
      const name = seg.slice(1)
      const val = opts.params?.[name] ?? positional[posIdx++]
      used.add(name)
      if (val === undefined) return null
      out.push(encodeURIComponent(val))
    } else {
      out.push(encodeURIComponent(seg))
    }
  }
  const qs = new URLSearchParams()
  for (const [k, v] of Object.entries(opts.params ?? {})) {
    if (!used.has(k)) qs.set(k, v)
  }
  const base = `/${out.join("/")}`
  const q = qs.toString()
  return q ? `${base}?${q}` : base
}

// In-memory structured filter (`where`) and row ordering (`compareRows`), matching the server's
// collection query semantics so live queries apply updates consistently on the client.
import { RookeryError } from "./errors"
import type { CollectionContribution } from "./generated"

const MAX_NODES = 256
const MAX_DEPTH = 16
const MAX_LIST = 1000

type Kind =
  | { k: "bytes" }
  | { k: "vector" }
  | { k: "text" }
  | { k: "richtext" }
  | { k: "integer" }
  | { k: "number" }
  | { k: "boolean" }
  | { k: "date" }
  | { k: "datetime" }
  | { k: "enum"; values: string[] }
  | { k: "json" }
  | { k: "ref"; to?: string }
  | { k: "refs"; to?: string }
  | { k: "uuid" }

type Bind =
  | { t: "null" }
  | { t: "text"; v: string }
  | { t: "num"; v: number }
  | { t: "bool"; v: boolean }
  | { t: "json"; v: unknown }
  | { t: "textArr"; v: string[] }

type Op = "eq" | "ne" | "in" | "nin" | "lt" | "lte" | "gt" | "gte" | "contains"

type Node =
  | { and: Node[] }
  | { or: Node[] }
  | { not: Node }
  | { field: string; kind: Kind; op: Op; one?: Bind; many?: Bind[] }

/** The shape of a collection the filter needs: its scope, options and fields. */
export type FilterCollection = Pick<CollectionContribution, "scope" | "trash" | "fields"> & {
  kind?: "regular" | "log"
}

export interface FilterField {
  name: string
  type:
    | "bytes"
    | "blob"
    | "vector"
    | "text"
    | "longtext"
    | "path"
    | "richtext"
    | "integer"
    | "sequence"
    | "number"
    | "boolean"
    | "date"
    | "datetime"
    | "enum"
    | "json"
    | "ref"
    | "refs"
  values?: Array<string | { value: string; title?: string; tone?: string }>
  to?: string
}

function invalid(message: string): RookeryError {
  return new RookeryError("INVALID", message)
}

function kindOf(f: Pick<FilterField, "type" | "values" | "to">): Kind {
  switch (f.type) {
    case "bytes":
      return { k: "bytes" }
    case "vector":
      return { k: "vector" }
    case "blob":
    case "text":
    case "longtext":
    case "path":
      return { k: "text" }
    case "richtext":
      return { k: "richtext" }
    case "integer":
    case "sequence":
      return { k: "integer" }
    case "number":
      return { k: "number" }
    case "boolean":
      return { k: "boolean" }
    case "date":
      return { k: "date" }
    case "datetime":
      return { k: "datetime" }
    case "enum":
      return { k: "enum", values: (f.values ?? []).map((v) => (typeof v === "string" ? v : v.value)) }
    case "json":
      return { k: "json" }
    case "ref":
      return { k: "ref", to: f.to }
    case "refs":
      return { k: "refs", to: f.to }
  }
}

/** The kind of a field or system field, by its name in rows. */
export function columnKind(coll: FilterCollection, name: string): Kind | undefined {
  const scope = coll.scope ?? "space"
  if (coll.kind === "log") {
    switch (name) {
      case "id":
      case "createdBy":
        return { k: "uuid" }
      case "seq":
        return { k: "integer" }
      case "createdAt":
        return { k: "datetime" }
      case "spaceId":
        return scope === "space" ? { k: "uuid" } : undefined
      case "userId":
        return scope === "user" ? { k: "uuid" } : undefined
    }
    const f = coll.fields.find((x) => x.name === name)
    return f ? kindOf(f as FilterField) : undefined
  }
  switch (name) {
    case "id":
      return { k: "uuid" }
    case "spaceId":
      return scope === "space" ? { k: "uuid" } : undefined
    case "userId":
      return scope === "user" ? { k: "uuid" } : undefined
    case "createdAt":
    case "updatedAt":
      return { k: "datetime" }
    case "deletedAt":
      return coll.trash ? { k: "datetime" } : undefined
    case "createdBy":
    case "updatedBy":
      return { k: "uuid" }
    case "rev":
      return { k: "integer" }
  }
  const f = coll.fields.find((x) => x.name === name)
  return f ? kindOf(f as FilterField) : undefined
}

const SCHEME = /^[a-z][a-z0-9+.-]{0,31}$/
const UUID = /^[0-9a-fA-F]{8}-?[0-9a-fA-F]{4}-?[0-9a-fA-F]{4}-?[0-9a-fA-F]{4}-?[0-9a-fA-F]{12}$/
const DATE = /^(\d{4})-(\d{2})-(\d{2})$/
const DATETIME = /^(\d{4})-(\d{2})-(\d{2})[Tt ](\d{2}):(\d{2}):(\d{2})(\.\d+)?([Zz]|[+-]\d{2}:\d{2})$/

function parseUri(s: string): string {
  const at = s.indexOf("://")
  if (at < 0) throw invalid(`not an entity URI: ${JSON.stringify(s)}`)
  const scheme = s.slice(0, at)
  const id = s.slice(at + 3)
  if (!SCHEME.test(scheme)) throw invalid(`bad scheme in URI ${JSON.stringify(s)}`)
  // biome-ignore lint/suspicious/noControlCharactersInRegex: control characters are refused
  if (!id || new TextEncoder().encode(id).length > 512 || /[\s\u0000-\u001f\u007f-\u009f]/u.test(id))
    throw invalid(`bad id in URI ${JSON.stringify(s)}`)
  return `${scheme}://${id}`
}

/** A reference as the store keeps it: a URI, or with `to` a bare id turned into `<to>://<id>`. */
export function normalizeRef(v: string, to?: string): string {
  if (v.includes("://")) {
    const u = parseUri(v)
    if (to && !u.startsWith(`${to}://`))
      throw invalid(`expected a ${to}:// reference, got ${JSON.stringify(v)}`)
    return u
  }
  if (to) return parseUri(`${to}://${v}`)
  throw invalid(`expected an entity URI, got ${JSON.stringify(v)}`)
}

function daysFromCivil(y: number, m: number, d: number): number {
  return Math.floor(Date.UTC(y, m - 1, d) / 86_400_000)
}

/** `YYYY-MM-DD` as a day number, or null if it is not a real date. */
export function parseDate(s: unknown): number | null {
  if (typeof s !== "string") return null
  const m = DATE.exec(s)
  if (!m) return null
  const [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])]
  const dt = new Date(Date.UTC(y, mo - 1, d))
  if (dt.getUTCFullYear() !== y || dt.getUTCMonth() !== mo - 1 || dt.getUTCDate() !== d) return null
  return daysFromCivil(y, mo, d)
}

/** An RFC 3339 date-time as microseconds since the epoch. */
export function parseDatetime(s: unknown): number | null {
  if (typeof s !== "string") return null
  const m = DATETIME.exec(s)
  if (!m) return null
  const [y, mo, d, h, mi, sec] = [1, 2, 3, 4, 5, 6].map((i) => Number(m[i]))
  if (mo! < 1 || mo! > 12 || d! < 1 || d! > 31 || h! > 23 || mi! > 59 || sec! > 60) return null
  if (parseDate(`${m[1]}-${m[2]}-${m[3]}`) === null) return null
  const frac = m[7] ? Number(`0${m[7]}`) : 0
  let offset = 0
  const z = m[8]!
  if (z !== "Z" && z !== "z") {
    const sign = z[0] === "-" ? -1 : 1
    offset = sign * (Number(z.slice(1, 3)) * 60 + Number(z.slice(4, 6)))
  }
  const secs = daysFromCivil(y!, mo!, d!) * 86400 + h! * 3600 + mi! * 60 + sec! - offset * 60
  return secs * 1_000_000 + Math.round(frac * 1_000_000)
}

function toBind(kind: Kind, v: unknown): Bind {
  if (v === null || v === undefined) return { t: "null" }
  const want = (what: string) => invalid(`expected ${what}, got ${JSON.stringify(v)}`)
  switch (kind.k) {
    case "bytes":
    case "vector":
      throw invalid("this field is only filterable for null")
    case "text":
      if (typeof v !== "string") throw want("a string")
      return { t: "text", v }
    case "richtext":
      if (typeof v !== "object" || Array.isArray(v)) throw want("a rich text document (an object)")
      return { t: "json", v }
    case "integer":
      if (typeof v !== "number" || !Number.isFinite(v) || !Number.isInteger(v) || Math.abs(v) >= 9e15)
        throw want("an integer")
      return { t: "num", v }
    case "number":
      if (typeof v !== "number" || !Number.isFinite(v)) throw want("a number")
      return { t: "num", v }
    case "boolean":
      if (typeof v !== "boolean") throw want("a boolean")
      return { t: "bool", v }
    case "date": {
      const d = parseDate(v)
      if (d === null) throw want("a date (YYYY-MM-DD)")
      return { t: "num", v: d }
    }
    case "datetime": {
      const t = parseDatetime(v)
      if (t === null) throw want("an RFC 3339 date-time")
      return { t: "num", v: t }
    }
    case "enum":
      if (typeof v !== "string") throw want("a string")
      if (!kind.values.includes(v))
        throw invalid(`${JSON.stringify(v)} is not one of ${kind.values.join(", ")}`)
      return { t: "text", v }
    case "json":
      return { t: "json", v }
    case "ref":
      if (typeof v !== "string") throw want("a reference")
      return { t: "text", v: normalizeRef(v, kind.to) }
    case "refs": {
      if (!Array.isArray(v)) throw want("a list of references")
      const out: string[] = []
      for (const item of v) {
        if (typeof item !== "string") throw want("a list of references")
        const r = normalizeRef(item, kind.to)
        if (!out.includes(r)) out.push(r)
      }
      return { t: "textArr", v: out }
    }
    case "uuid":
      if (typeof v !== "string" || !UUID.test(v)) throw want("a UUID")
      return { t: "text", v: canonicalUuid(v) }
  }
}

function canonicalUuid(s: string): string {
  const h = s.replace(/-/g, "").toLowerCase()
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`
}

const ORDERED = (k: Kind) => !["richtext", "json", "refs", "boolean", "bytes", "vector"].includes(k.k)

function parseCmp(field: string, kind: Kind, op: string, v: unknown): Node {
  const err = (m: string) => invalid(`${field}: ${m}`)
  const wrap = <T>(f: () => T): T => {
    try {
      return f()
    } catch (e) {
      throw err((e as Error).message)
    }
  }
  if (["bytes", "vector"].includes(kind.k) && !(["eq", "ne"].includes(op) && isNull(v)))
    throw err("only eq/ne null is supported for bytes and vector")
  switch (op) {
    case "eq":
    case "ne":
      if (kind.k === "refs") throw err("use contains on a refs field")
      if (Array.isArray(v) && kind.k !== "json") throw err("a list needs the in operator")
      return { field, kind, op, one: wrap(() => toBind(kind, v)) }
    case "in":
    case "nin": {
      if (kind.k === "refs" || kind.k === "json" || kind.k === "richtext")
        throw err(`in does not apply to ${kind.k} fields`)
      if (!Array.isArray(v)) throw err("in takes a list")
      if (v.length > MAX_LIST) throw err(`at most ${MAX_LIST} values`)
      return { field, kind, op, many: v.map((i) => wrap(() => toBind(kind, i))) }
    }
    case "lt":
    case "lte":
    case "gt":
    case "gte":
      if (!ORDERED(kind)) throw err(`cannot compare ${kind.k === "uuid" ? "id" : kind.k} fields`)
      if (v === null || v === undefined) throw err("cannot compare with null")
      return { field, kind, op, one: wrap(() => toBind(kind, v)) }
    case "contains": {
      if (kind.k !== "refs") throw err("contains applies to refs fields")
      const items = Array.isArray(v) ? v : [v]
      const out = items.map((i) => {
        if (typeof i !== "string") throw err("contains takes a reference or a list of them")
        return wrap(() => normalizeRef(i, kind.to))
      })
      return { field, kind, op, one: { t: "textArr", v: out } }
    }
    default:
      throw err(`unknown operator ${op}`)
  }
}

function parseNode(coll: FilterCollection, v: unknown, depth: number, count: { n: number }): Node {
  if (depth > MAX_DEPTH) throw invalid("where is nested too deeply")
  if (!v || typeof v !== "object" || Array.isArray(v)) throw invalid("where must be an object")
  const nodes: Node[] = []
  for (const [key, val] of Object.entries(v as Record<string, unknown>)) {
    if (++count.n > MAX_NODES) throw invalid("where is too large")
    if (key === "and" || key === "or") {
      if (!Array.isArray(val)) throw invalid(`${key} takes a list`)
      const parsed = val.map((i) => parseNode(coll, i, depth + 1, count))
      nodes.push(key === "and" ? { and: parsed } : { or: parsed })
    } else if (key === "not") {
      nodes.push({ not: parseNode(coll, val, depth + 1, count) })
    } else {
      const kind = columnKind(coll, key)
      if (!kind) throw invalid(`unknown field ${key} in where`)
      if (val && typeof val === "object" && !Array.isArray(val)) {
        const ops = Object.entries(val as Record<string, unknown>)
        if (!ops.length) throw invalid(`${key}: empty operator object`)
        for (const [op, operand] of ops) nodes.push(parseCmp(key, kind, op, operand))
      } else {
        nodes.push(parseCmp(key, kind, "eq", val))
      }
    }
  }
  return nodes.length === 1 ? nodes[0]! : { and: nodes }
}

/** Compares strings by Unicode code point, as `COLLATE "C"` and Rust do. */
export function compareCodePoints(a: string, b: string): number {
  const n = Math.min(a.length, b.length)
  for (let i = 0; i < n; i++) {
    const x = a.codePointAt(i)!
    const y = b.codePointAt(i)!
    if (x !== y) return x < y ? -1 : 1
    if (x > 0xffff) i++
  }
  return a.length === b.length ? 0 : a.length < b.length ? -1 : 1
}

function jsonEq(a: unknown, b: unknown): boolean {
  if (typeof a === "number" && typeof b === "number") return a === b
  if (Array.isArray(a) && Array.isArray(b)) return a.length === b.length && a.every((x, i) => jsonEq(x, b[i]))
  if (a && b && typeof a === "object" && typeof b === "object" && !Array.isArray(a) && !Array.isArray(b)) {
    const ka = Object.keys(a as object)
    const kb = Object.keys(b as object)
    return (
      ka.length === kb.length && ka.every((k) => k in (b as object) && jsonEq((a as any)[k], (b as any)[k]))
    )
  }
  return a === b
}

function compare(kind: Kind, v: unknown, b: Bind): number | null {
  switch (b.t) {
    case "text": {
      if (typeof v !== "string") return null
      const x = kind.k === "uuid" ? (UUID.test(v) ? canonicalUuid(v) : null) : v
      if (x === null) return null
      return compareCodePoints(x, b.v)
    }
    case "num": {
      let x: number | null
      if (kind.k === "date") x = parseDate(v)
      else if (kind.k === "datetime") x = parseDatetime(v)
      else x = typeof v === "number" ? v : null
      if (x === null) return null
      return x < b.v ? -1 : x > b.v ? 1 : 0
    }
    case "bool":
      if (typeof v !== "boolean") return null
      return v === b.v ? 0 : v ? 1 : -1
    case "json":
      if (kind.k === "json" || kind.k === "richtext") return jsonEq(v, b.v) ? 0 : null
      return null
    default:
      return null
  }
}

function equal(kind: Kind, v: unknown, b: Bind): boolean {
  if ((kind.k === "json" || kind.k === "richtext") && b.t === "json") return jsonEq(v, b.v)
  return compare(kind, v, b) === 0
}

const isNull = (v: unknown) => v === null || v === undefined

function evalNode(n: Node, row: Record<string, unknown>): boolean {
  if ("and" in n) return n.and.every((x) => evalNode(x, row))
  if ("or" in n) return n.or.some((x) => evalNode(x, row))
  if ("not" in n) return !evalNode(n.not, row)
  const v = row[n.field]
  const inList = (items: Bind[]) =>
    items.some((b) => (b.t === "null" ? isNull(v) : !isNull(v) && equal(n.kind, v, b)))
  switch (n.op) {
    case "eq":
      return n.one!.t === "null" ? isNull(v) : !isNull(v) && equal(n.kind, v, n.one!)
    case "ne":
      return n.one!.t === "null" ? !isNull(v) : isNull(v) || !equal(n.kind, v, n.one!)
    case "in":
      return inList(n.many!)
    case "nin":
      return !inList(n.many!)
    case "contains": {
      if (!Array.isArray(v)) return false
      const want = (n.one as { v: string[] }).v
      return want.every((w) => v.some((h) => h === w))
    }
    default: {
      if (isNull(v)) return false
      const o = compare(n.kind, v, n.one!)
      if (o === null) return false
      if (n.op === "lt") return o < 0
      if (n.op === "lte") return o <= 0
      if (n.op === "gt") return o > 0
      return o >= 0
    }
  }
}

export interface Filter {
  matches(row: Record<string, unknown>): boolean
}

/** Parses `where` for a collection. Unknown fields and operators throw INVALID, as on the server. */
export function parseFilter(coll: FilterCollection, where: unknown): Filter {
  const node = where === undefined || where === null ? { and: [] } : parseNode(coll, where, 0, { n: 0 })
  return { matches: (row) => evalNode(node, row) }
}

export type OrderBy = Array<string | [string, "asc" | "desc"]>

/**
 * A comparator for rows in a query's order: its `orderBy`, then `id`, with Postgres's null placement
 * (nulls last ascending, first descending).
 */
export function compareRows(
  coll: FilterCollection,
  orderBy: OrderBy | undefined,
): (a: Record<string, unknown>, b: Record<string, unknown>) => number {
  const keys = (orderBy ?? [])
    .map((o) => (typeof o === "string" ? { name: o, desc: false } : { name: o[0], desc: o[1] === "desc" }))
    .filter((k) => k.name !== "id")
    .map((k) => ({ ...k, kind: columnKind(coll, k.name) ?? ({ k: "text" } as Kind) }))
  keys.push({ name: "id", desc: false, kind: { k: "uuid" } })
  const value = (kind: Kind, v: unknown): number | string | boolean | null => {
    if (isNull(v)) return null
    if (kind.k === "date") return parseDate(v)
    if (kind.k === "datetime") return parseDatetime(v)
    if (kind.k === "uuid") return typeof v === "string" ? v.toLowerCase() : null
    if (typeof v === "number" || typeof v === "string" || typeof v === "boolean") return v
    return null
  }
  return (a, b) => {
    for (const k of keys) {
      const x = value(k.kind, a[k.name])
      const y = value(k.kind, b[k.name])
      let o: number
      if (x === null && y === null) o = 0
      else if (x === null) o = 1
      else if (y === null) o = -1
      else if (typeof x === "string" && typeof y === "string")
        o = k.kind.k === "uuid" ? compareCodePoints(x, y) : x.localeCompare(y)
      else o = x < y ? -1 : x > y ? 1 : 0
      if (o !== 0) return k.desc ? -o : o
    }
    return 0
  }
}

// The collection API shared by host and client halves. Rows are typed from the manifest by
// rookery-env.d.ts (`RookeryEnv.collections`).
import { RookeryError } from "./errors"
import type {
  Backlink,
  BacklinkList,
  DataAggregateResult,
  DataBatchResult,
  DataCount,
  DataImportResult,
  DataPage,
  DataRow,
  DataTailResult,
  GroupBucket,
  HistoryEntry,
  HistoryList,
  ImportFailure,
  Reference,
  ReferenceList,
  BatchOp as WireBatchOp,
} from "./generated"
import type { Collections } from "./types"

/** A ProseMirror document, as `richtext` fields store it. */
export interface RichText {
  type: "doc"
  content?: RichTextNode[]
}

export interface RichTextNode {
  type: string
  text?: string
  attrs?: Record<string, unknown>
  content?: RichTextNode[]
  marks?: Array<{ type: string; attrs?: Record<string, unknown> }>
}

/** The fields every row has. */
export interface SystemFields {
  id: string
  createdAt: string
  updatedAt: string
  createdBy: string | null
  updatedBy: string | null
  rev: number
  _distance?: number
}

export type Row = SystemFields & Record<string, unknown>

export type Where<T = Row> = {
  [K in keyof T]?: T[K] | Condition<T[K]>
} & { and?: Where<T>[]; or?: Where<T>[]; not?: Where<T> }

export interface Condition<V> {
  eq?: V | null
  ne?: V | null
  in?: Array<V | null>
  nin?: Array<V | null>
  lt?: V
  lte?: V
  gt?: V
  gte?: V
  contains?: string | string[]
}

export type OrderKey<T> = (keyof T & string) | [keyof T & string, "asc" | "desc"]

export interface Query<T = Row> {
  where?: Where<T>
  orderBy?: OrderKey<T>[]
  limit?: number
  after?: string
  search?: string
  spaces?: string[]
  trashed?: boolean
  select?: Array<keyof T & string>
  nearest?: { field: keyof T & string; to: Float32Array | number[]; maxDistance?: number }
  include?: Array<keyof T & string>
}

export interface Page<T> {
  rows: T[]
  next: string | null
  included?: Record<string, Row>
}

export interface ScanQuery<T = Row> {
  where?: Where<T>
  select?: Array<keyof T & string>
  spaces?: string[]
  trashed?: boolean
  limit?: number
  after?: string
}

export type Measure<T = Row> =
  | { count: true }
  | { countDistinct: keyof T & string }
  | { sum: keyof T & string }
  | { avg: keyof T & string }
  | { min: keyof T & string }
  | { max: keyof T & string }

export interface AggregateQuery<T = Row> {
  where?: Where<T>
  groupBy?: Array<(keyof T & string) | { field: keyof T & string; by: GroupBucket }>
  measures: Record<string, Measure<T>>
  orderBy?: Array<string | [string, "asc" | "desc"]>
  limit?: number
  spaces?: string[]
}

export interface ImportResult {
  inserted: number
  updated: number
  failed: ImportFailure[]
}

export interface ImportOptions {
  mode?: "insert" | "upsert"
  on?: string[]
}

export interface TailOptions<T = Row> {
  where?: Where<T>
  limit?: number
  select?: Array<keyof T & string>
  spaces?: string[]
}

export const IMPORT_CHUNK = 500

export type Insert<T> = Partial<Omit<T, keyof SystemFields | "spaceId" | "userId" | "deletedAt" | "seq">>
export type Patch<T> = Insert<T>

export type FieldOps = Record<string, { inc: number } | { add: string[] } | { remove: string[] }>

export interface BatchOptions {
  if?: { lease: { key: string; token: number } }
}

export type BatchOp =
  | { op: "insert"; collection?: string; values: Record<string, unknown> }
  | {
      op: "update"
      collection?: string
      id: string
      patch: Record<string, unknown>
      rev?: number
      ops?: FieldOps
    }
  | { op: "upsert"; collection?: string; values: Record<string, unknown>; on: string[] }
  | { op: "publish"; topic: string; payload: unknown }
  | { op: "enqueue"; job: string; params: Record<string, unknown> }
  | { op: "delete"; collection?: string; id: string }
  | { op: "restore"; collection?: string; id: string }

export interface Collection<T = Row> {
  find(query?: Query<T>): Promise<Page<T>>
  scan(query?: ScanQuery<T>): AsyncGenerator<T, void, undefined>
  scanPages(query?: ScanQuery<T>): AsyncGenerator<Page<T>, void, undefined>
  aggregate(query: AggregateQuery<T>): Promise<Array<Record<string, unknown>>>
  import(rows: Iterable<Insert<T>> | AsyncIterable<Insert<T>>, opts?: ImportOptions): Promise<ImportResult>
  get(id: string, opts?: { trashed?: boolean }): Promise<T | null>
  count(query?: Pick<Query<T>, "where" | "search" | "spaces" | "trashed">): Promise<number>
  search(text: string, opts?: { where?: Where<T>; limit?: number }): Promise<Page<T>>
  insert(values: Insert<T>): Promise<T>
  update(id: string, patch: Patch<T>, opts?: { rev?: number; ops?: FieldOps }): Promise<T>
  upsert(values: Insert<T>, opts: { on: string[] }): Promise<T>
  delete(id: string): Promise<T | null>
  restore(id: string): Promise<T>
  purge(id: string): Promise<void>
  batch(ops: BatchOp[], opts?: BatchOptions): Promise<Array<Row | null>>
  history(id: string, opts?: { limit?: number }): Promise<HistoryEntry[]>
}

export interface LogCollection<T = Row> {
  find(query?: Omit<Query<T>, "trashed" | "search">): Promise<Page<T>>
  scan(query?: Omit<ScanQuery<T>, "trashed">): AsyncGenerator<T, void, undefined>
  scanPages(query?: Omit<ScanQuery<T>, "trashed">): AsyncGenerator<Page<T>, void, undefined>
  aggregate(query: AggregateQuery<T>): Promise<Array<Record<string, unknown>>>
  import(rows: Iterable<Insert<T>> | AsyncIterable<Insert<T>>): Promise<ImportResult>
  get(id: string): Promise<T | null>
  count(query?: Pick<Query<T>, "where" | "spaces">): Promise<number>
  insert(values: Insert<T>): Promise<T>
  tail(fromSeq: number, opts?: TailOptions<T>): Promise<{ rows: T[]; seq: number }>
}

export interface Refs {
  to(uri: string): Promise<Backlink[]>
  from(uri: string): Promise<Reference[]>
}

export type CollectionsOf<C> = {
  readonly [K in keyof C]: C[K] extends { rev: number } ? Collection<C[K]> : LogCollection<C[K]>
}

export type Call = (method: string, params: Record<string, unknown>) => Promise<unknown>

export const RESERVED = new Set(["refs", "changes", "batch", "space", "then"])

export function createCollection<T>(
  transport: Call,
  name: string,
  definition?: (name: string) => BinaryCollection | undefined,
): Collection<T> & LogCollection<T> {
  const call: Call = async (method, params) => {
    const wire = { ...params }
    const def = definition?.(name)
    if (wire.values) wire.values = binaryRow(wire.values, def, true)
    if (wire.patch) wire.patch = binaryRow(wire.patch, def, true)
    const batchOps = wire.ops as Array<Record<string, unknown>> | undefined
    if (method === "data.batch" && batchOps) {
      wire.ops = batchOps.map((op) => {
        const opDef = definition?.(op.collection as string)
        return {
          ...op,
          ...(op.values ? { values: binaryRow(op.values, opDef, true) } : {}),
          ...(op.patch ? { patch: binaryRow(op.patch, opDef, true) } : {}),
        }
      })
    }
    if (method === "data.import" && Array.isArray(wire.rows))
      wire.rows = wire.rows.map((row) => binaryRow(row, def, true))
    const nearest = (wire.query as Query | undefined)?.nearest
    if (nearest?.to instanceof Float32Array)
      wire.query = { ...(wire.query as object), nearest: { ...nearest, to: Array.from(nearest.to) } }
    rejectNativeArrays(wire)
    const result = (await transport(method, wire)) as any
    if (method === "data.aggregate" || method === "data.import") return result
    if (result?.row) return { ...result, row: binaryRow(result.row, def, false) }
    if (result?.rows)
      return {
        ...result,
        rows: result.rows.map((row: unknown, i: number) =>
          binaryRow(
            row,
            method === "data.batch" ? definition?.(batchOps?.[i]?.collection as string) : def,
            false,
          ),
        ),
      }
    return result
  }
  const c = { collection: name }
  async function* scanPages(query?: ScanQuery<any>): AsyncGenerator<Page<any>, void, undefined> {
    let after = query?.after
    for (;;) {
      const page = (await call("data.scan", { ...c, query: { ...query, after } })) as DataPage
      yield page as Page<any>
      if (!page.next) return
      after = page.next
    }
  }
  async function* scan(query?: ScanQuery<any>): AsyncGenerator<any, void, undefined> {
    for await (const page of scanPages(query)) yield* page.rows
  }
  const importRows = async (
    rows: Iterable<Record<string, unknown>> | AsyncIterable<Record<string, unknown>>,
    opts?: ImportOptions,
  ): Promise<ImportResult> => {
    const total: ImportResult = { inserted: 0, updated: 0, failed: [] }
    let done = 0
    let chunk: Array<Record<string, unknown>> = []
    const flush = async () => {
      if (!chunk.length) return
      try {
        const r = (await call("data.import", {
          ...c,
          rows: chunk,
          mode: opts?.mode ?? "insert",
          on: opts?.on,
        })) as DataImportResult
        total.inserted += r.inserted
        total.updated += r.updated
        for (const f of r.failed) total.failed.push({ ...f, index: f.index + done })
      } catch (e) {
        if (e instanceof RookeryError)
          throw new RookeryError(e.code, e.message, {
            ...(e.details as object),
            imported: { ...total, rowsDone: done },
          })
        throw e
      }
      done += chunk.length
      chunk = []
    }
    for await (const row of rows) {
      chunk.push(row)
      if (chunk.length === IMPORT_CHUNK) await flush()
    }
    await flush()
    return total
  }
  const collection: Collection<any> & Pick<LogCollection<any>, "tail"> = {
    find: async (query) => (await call("data.find", { ...c, query: query ?? {} })) as DataPage as any,
    scan,
    scanPages,
    aggregate: async (query) =>
      ((await call("data.aggregate", { ...c, ...query })) as DataAggregateResult).rows,
    import: importRows as any,
    tail: async (fromSeq, opts) => {
      const r = (await call("data.tail", { ...c, fromSeq, ...opts })) as DataTailResult
      return { rows: r.rows, seq: r.seq }
    },
    get: async (id, opts) =>
      ((await call("data.get", { ...c, id, trashed: !!opts?.trashed })) as DataRow).row as any,
    count: async (query) => ((await call("data.count", { ...c, query: query ?? {} })) as DataCount).count,
    search: async (text, opts) =>
      (await call("data.search", { ...c, text, where: opts?.where, limit: opts?.limit })) as DataPage as any,
    insert: async (values) => ((await call("data.insert", { ...c, values })) as DataRow).row as any,
    update: async (id, patch, opts) =>
      ((await call("data.update", { ...c, id, patch, rev: opts?.rev, ops: opts?.ops })) as DataRow)
        .row as any,
    upsert: async (values, opts) =>
      ((await call("data.upsert", { ...c, values, on: opts.on })) as DataRow).row as any,
    delete: async (id) => ((await call("data.delete", { ...c, id })) as DataRow).row as any,
    restore: async (id) => ((await call("data.restore", { ...c, id })) as DataRow).row as any,
    purge: async (id) => {
      await call("data.purge", { ...c, id })
    },
    batch: async (ops, opts) => {
      const wire = ops.map((o) =>
        "topic" in o || "job" in o ? o : { ...o, collection: o.collection ?? name },
      ) as WireBatchOp[]
      return (
        (await call("data.batch", {
          ops: wire,
          if: opts?.if ? { lease: { key: opts.if.lease.key, token: opts.if.lease.token } } : undefined,
        })) as DataBatchResult
      ).rows as any
    },
    history: async (id, opts) =>
      ((await call("data.history", { ...c, id, limit: opts?.limit })) as HistoryList).entries,
  }
  return collection as unknown as Collection<T> & LogCollection<T>
}

export function createRefs(call: Call): Refs {
  return {
    to: async (uri) => ((await call("data.refsTo", { uri })) as BacklinkList).backlinks,
    from: async (uri) => ((await call("data.refsFrom", { uri })) as ReferenceList).references,
  }
}

export function collectionsProxy<X extends object, C = Collections>(
  make: (name: string) => unknown,
  extra: X,
): CollectionsOf<C> & X {
  const cache = new Map<string, unknown>()
  return new Proxy(extra as CollectionsOf<C> & X, {
    get: (target, name) => {
      if (typeof name !== "string") return undefined
      if (name in target) return (target as any)[name]
      if (RESERVED.has(name)) return undefined
      if (!cache.has(name)) cache.set(name, make(name))
      return cache.get(name)
    },
  })
}

export function plainText(doc: RichText | null | undefined): string {
  const out: string[] = []
  const walk = (n: RichTextNode) => {
    if (n.type === "text" && n.text) out.push(n.text)
    else if (n.type === "mention") out.push(`@${String(n.attrs?.label ?? n.attrs?.uri ?? "")}`)
    else if (n.type === "hardBreak") out.push("\n")
    for (const c of n.content ?? []) walk(c)
    if (n.type === "paragraph") out.push("\n")
  }
  if (doc) walk(doc as RichTextNode)
  return out.join("").replace(/\n$/, "")
}

export function mentions(doc: RichText | null | undefined): string[] {
  const out: string[] = []
  const walk = (n: RichTextNode) => {
    if (n.type === "mention" && typeof n.attrs?.uri === "string" && !out.includes(n.attrs.uri))
      out.push(n.attrs.uri)
    for (const c of n.content ?? []) walk(c)
  }
  if (doc) walk(doc as RichTextNode)
  return out
}

export interface MigrationTx {
  rows(collection: string, opts?: { after?: string; limit?: number }): Promise<Array<Record<string, any>>>
  each(collection: string): AsyncGenerator<Record<string, any>>
  legacy(table: string): AsyncGenerator<Record<string, any>>
  insert(
    collection: string,
    values: Record<string, unknown>,
    scope?: { space?: string; user?: string },
  ): Promise<Record<string, any>>
  update(collection: string, id: string, patch: Record<string, unknown>): Promise<Record<string, any>>
  delete(collection: string, id: string): Promise<void>
  log(...args: unknown[]): void
}

interface BinaryCollection {
  fields: Array<{ name: string; type: string }>
}

/** Convert only declared binary fields; nested JSON values keep the JSON data model. */
export function binaryRow(row: unknown, def: BinaryCollection | undefined, encode: boolean): unknown {
  if (!row || typeof row !== "object") return row
  if (!def) {
    if (encode) rejectNativeArrays(row)
    return row
  }
  const out = { ...row } as Record<string, unknown>
  for (const field of def.fields) {
    const value = out[field.name]
    if (value == null) continue
    if (field.type === "bytes") {
      if (encode && value instanceof Uint8Array) {
        let s = ""
        for (let start = 0; start < value.length; start += 8192)
          s += String.fromCharCode(...value.subarray(start, start + 8192))
        out[field.name] = btoa(s)
      } else if (!encode && typeof value === "string") {
        out[field.name] = Uint8Array.from(atob(value), (c) => c.charCodeAt(0))
      }
    } else if (field.type === "vector") {
      if (encode && value instanceof Float32Array) out[field.name] = Array.from(value)
      else if (!encode && Array.isArray(value)) out[field.name] = new Float32Array(value)
    }
  }
  if (encode) rejectNativeArrays(out)
  return out
}

function rejectNativeArrays(value: unknown, seen = new WeakSet<object>()): void {
  if (!value || typeof value !== "object") return
  if (ArrayBuffer.isView(value) || value instanceof ArrayBuffer)
    throw new RookeryError("INVALID", "native binary is only allowed in declared bytes/vector fields")
  if (seen.has(value)) throw new RookeryError("INVALID", "stored data cannot contain cycles")
  seen.add(value)
  for (const child of Object.values(value)) rejectNativeArrays(child, seen)
  seen.delete(value)
}

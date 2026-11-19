// Live queries in the client: fetches the first page via data.live, applies data.changed updates
// using the client-side filter and comparator, resumes from the last seen seq after reconnect, and
// caches recent query snapshots briefly so remounting views render immediately.
import { batch, createSignal, getOwner, onCleanup } from "solid-js"
import { createStore } from "solid-js/store"
import type { Row } from "../data"
import { RookeryError } from "../errors"
import { compareRows, type FilterCollection, type OrderBy, parseFilter } from "../filter"
import type { Change, DataChanged, DataPage, LiveStarted } from "../generated"
import type { Disposable } from "../types"

export interface LiveOptions {
  call(method: string, params: Record<string, unknown>): Promise<unknown>
  onNotification(method: string, fn: (params: any) => void): Disposable
  onReconnect(fn: () => void): Disposable
}

/** A live result, used like a Solid resource: call it for the current rows. */
export interface Live<T> {
  (): T[]
  readonly loading: boolean
  readonly error: RookeryError | undefined
  readonly hasMore: boolean
  readonly included: Record<string, Row>
  /**
   * The server ended the subscription, such as when the caller lost access to the space it follows: the
   * rows stay as they were, `error` says why, and no more changes come until `refetch` starts again.
   */
  readonly ended: boolean
  /** Calls `fn` with the reason when the server ends the subscription. */
  onEnded(fn: (error: RookeryError) => void): Disposable
  refetch(): void
  dispose(): void
}

export interface LiveQuery {
  where?: unknown
  orderBy?: OrderBy
  limit?: number
  search?: string
  spaces?: string[]
  trashed?: boolean
  select?: string[]
  nearest?: { field: string; to: number[] | Float32Array; maxDistance?: number }
  include?: string[]
}

const REFETCH_DEBOUNCE_MS = 150
const PAGE = 1000
const LAST_MS = 5 * 60_000
const LAST_MAX = 300

interface Last {
  rows: Row[]
  hasMore: boolean
  at: number
}

const last = new Map<string, Last>()

/** Clears the in-memory recent query cache (useful in tests or on sign-out). */
export function clearLiveCache() {
  last.clear()
}

function lastRows(key: string): Last | undefined {
  const hit = last.get(key)
  if (!hit) return undefined
  if (Date.now() - hit.at > LAST_MS) {
    last.delete(key)
    return undefined
  }
  return hit
}

function keepRows(key: string, rows: Row[], hasMore: boolean) {
  last.delete(key)
  if (rows.length > PAGE) return
  last.set(key, { rows, hasMore, at: Date.now() })
  if (last.size > LAST_MAX) last.delete(last.keys().next().value!)
}

export function createLive<T extends Row>(
  opts: LiveOptions,
  target: { plugin: string; collection: string; space?: string | null },
  coll: FilterCollection,
  query: LiveQuery = {},
): Live<T> {
  const key = JSON.stringify([target.plugin, target.collection, target.space ?? null, query])
  const before = lastRows(key)
  const [rows, setRowsRaw] = createSignal<T[]>((before?.rows as T[]) ?? [], { equals: false })
  const [state, setState] = createStore<{
    loading: boolean
    error?: RookeryError
    hasMore: boolean
    ended: boolean
  }>({
    loading: !before,
    hasMore: before?.hasMore ?? false,
    ended: false,
  })
  const endedListeners = new Set<(error: RookeryError) => void>()
  let fresh = false
  const setRows = (f: () => T[]) => {
    const next = setRowsRaw(f)
    if (fresh) keepRows(key, next, state.hasMore)
    return next
  }
  const filter = parseFilter(coll, query.where)
  const excluded = new Set(
    coll.fields
      .filter((f) =>
        query.select ? !query.select.includes(f.name) : f.type === "bytes" || f.type === "vector",
      )
      .map((f) => f.name),
  )
  const needsServerFilter = (value: unknown): boolean => {
    if (!value || typeof value !== "object") return false
    return Object.entries(value).some(([k, child]) => excluded.has(k) || needsServerFilter(child))
  }
  const orderBy = (query.orderBy ?? []).map((o) => (typeof o === "string" ? o : o[0]))
  const refetchOnChange =
    !!query.search ||
    !!query.nearest ||
    !!query.include?.length ||
    needsServerFilter(query.where) ||
    orderBy.some((f) => excluded.has(f))
  const cmp = compareRows(coll, query.orderBy)
  const [included, setIncluded] = createSignal<Record<string, Row>>({})
  const limit = query.limit ?? 50
  let sub: string | undefined
  let seq = 0
  let disposed = false
  let buffer: DataChanged[] | undefined
  let timer: ReturnType<typeof setTimeout> | undefined

  const wireQuery = {
    ...query,
    ...(query.nearest && query.nearest.to instanceof Float32Array
      ? { nearest: { ...query.nearest, to: Array.from(query.nearest.to) } }
      : {}),
  }
  const params = () => ({
    plugin: target.plugin,
    collection: target.collection,
    ...(target.space ? { space: target.space } : {}),
    query: { ...wireQuery, limit: Math.min(limit, PAGE) },
  })

  const readRest = async (first: T[], next: string | null | undefined) => {
    let resultRows = first
    let cursor = next ?? null
    if (limit <= PAGE || !cursor || resultRows.length >= limit) return { rows: resultRows, next: cursor }
    const seen = new Map(resultRows.map((r, i) => [r.id, i]))
    while (cursor && resultRows.length < limit && !disposed) {
      const page = (await opts.call("data.find", {
        ...params(),
        query: { ...wireQuery, limit: Math.min(PAGE, limit - resultRows.length), after: cursor },
      })) as DataPage
      if (page.included) setIncluded((old) => ({ ...old, ...(page.included as Record<string, Row>) }))
      if (resultRows === first) resultRows = first.slice()
      for (const r of page.rows as T[]) {
        const at = seen.get(r.id)
        if (at === undefined) {
          seen.set(r.id, resultRows.length)
          resultRows.push(r)
        } else if ((resultRows[at]!.rev as number) < (r.rev as number)) resultRows[at] = r
      }
      cursor = page.next ?? null
    }
    if (resultRows !== first) resultRows.sort(cmp)
    return { rows: resultRows, next: cursor }
  }

  const refetchSoon = () => {
    if (timer) return
    timer = setTimeout(() => {
      timer = undefined
      if (!disposed) void refetch()
    }, REFETCH_DEBOUNCE_MS)
  }

  const apply = (list: T[], c: Change): T[] => {
    const at = list.findIndex((r) => r.id === c.id)
    const row = c.row as T | null
    const visible =
      !!row && (query.trashed ? row.deletedAt != null : row.deletedAt == null) && filter.matches(row)
    if (!visible) {
      if (at < 0) return list
      const next = list.slice()
      next.splice(at, 1)
      if (state.hasMore) refetchSoon()
      return next
    }
    if (at >= 0 && (list[at]!.rev as number) > (row!.rev as number)) return list
    const next = list.slice()
    if (at >= 0) next.splice(at, 1)
    const tail = next[next.length - 1]
    if (state.hasMore && tail && cmp(row!, tail) > 0) {
      if (at >= 0) refetchSoon()
      return next
    }
    let i = next.findIndex((r) => cmp(row!, r) < 0)
    if (i < 0) i = next.length
    next.splice(i, 0, row!)
    if (next.length > limit) {
      next.pop()
      setState("hasMore", true)
    }
    return next
  }

  const handle = (n: DataChanged) => {
    if (n.subscription !== sub || state.ended) return
    if (n.ended) {
      // The server sends nothing more on this subscription; nothing is undone of what is shown.
      const error = n.error
        ? new RookeryError(n.error.code, n.error.message, n.error.details)
        : new RookeryError("UNAVAILABLE", "the live query ended")
      setState({ ended: true, error, loading: false })
      for (const fn of endedListeners) fn(error)
      return
    }
    if (n.reset !== undefined) {
      void refetch()
      return
    }
    if (refetchOnChange) {
      for (const c of n.changes) seq = Math.max(seq, c.seq)
      refetchSoon()
      return
    }
    let list = rows()
    for (const c of n.changes) {
      if (c.seq <= seq) continue
      seq = c.seq
      list = apply(list, c)
    }
    setRows(() => list)
  }

  const off = opts.onNotification("data.changed", (n: DataChanged) => {
    if (buffer) buffer.push(n)
    else handle(n)
  })

  const start = async (resume: boolean) => {
    const old = sub
    buffer = []
    try {
      const r = (await opts.call("data.live", {
        ...params(),
        ...(resume ? { after: seq } : {}),
      })) as LiveStarted
      if (disposed) {
        opts.call("data.unlive", { subscription: r.subscription }).catch(() => {})
        return
      }
      sub = r.subscription
      if (r.rows) {
        const all = await readRest(r.rows as T[], r.next)
        if (disposed) return
        seq = r.seq
        fresh = true
        setIncluded((r.included as Record<string, Row> | undefined) ?? {})
        const next = sameRows(rows(), all.rows)
        batch(() => {
          setState({ loading: false, error: undefined, hasMore: !!all.next, ended: false })
          if (next !== rows()) setRows(() => next)
          else keepRows(key, next, state.hasMore)
        })
      }
      const pending = buffer
      buffer = undefined
      for (const n of pending) handle(n)
    } catch (e) {
      buffer = undefined
      setState({ loading: false, error: RookeryError.from(e) })
    } finally {
      if (old && old !== sub) opts.call("data.unlive", { subscription: old }).catch(() => {})
    }
  }

  const refetch = () => start(false)

  const offReconnect = opts.onReconnect(() => {
    if (disposed || state.ended) return
    sub = undefined
    void start(fresh)
  })

  const dispose = () => {
    if (disposed) return
    disposed = true
    off.dispose()
    offReconnect.dispose()
    if (timer) clearTimeout(timer)
    endedListeners.clear()
    if (sub) opts.call("data.unlive", { subscription: sub }).catch(() => {})
  }
  if (getOwner()) onCleanup(dispose)

  void refetch()

  const live = (() => rows()) as Live<T>
  Object.defineProperties(live, {
    loading: { get: () => state.loading },
    error: { get: () => state.error },
    hasMore: { get: () => state.hasMore },
    included: { get: () => included() },
    ended: { get: () => state.ended },
  })
  ;(live as any).onEnded = (fn: (error: RookeryError) => void): Disposable => {
    endedListeners.add(fn)
    return { dispose: () => void endedListeners.delete(fn) }
  }
  ;(live as any).refetch = () => void refetch()
  ;(live as any).dispose = dispose
  return live
}

function sameRows<T extends Row>(shown: T[], page: T[]): T[] {
  if (!shown.length) return page
  const byId = new Map(shown.map((r) => [r.id, r]))
  let changed = shown.length !== page.length
  const out = page.map((r, i) => {
    const old = byId.get(r.id)
    const keep = old && old.rev === r.rev ? old : r
    if (keep !== shown[i]) changed = true
    return keep
  })
  return changed ? out : shown
}

export type { DataPage }

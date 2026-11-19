// A plugin's public API, declared in one side-effect-free module (`docs/plugins.md`, "Interfaces"):
//
//   // interface.ts
//   import { defineInterface, z } from "@rookery/sdk"
//   export default defineInterface({
//     services: {
//       greeter: {
//         greet: {
//           params: z.object({ name: z.string() }),
//           result: z.object({ text: z.string() }),
//         },
//       },
//     },
//     events: { "greeter.greeted": z.object({ name: z.string() }) },
//   })
//
// A method with `stream: true` answers with a stream: its `result` is the schema of each chunk.
// The host half provides a service from it, and the extension host writes interface.json and
// interface.d.ts from it, which dependents are typed from.
import { RookeryError } from "./errors"
import type { Ctx } from "./host/types"

/** A schema a method's params or result is checked with: zod's, or any with `parse`. */
export interface Schema<T = unknown> {
  parse(input: unknown): T
}

/** One method of a service: the schemas of its params and of its result. */
export interface MethodDecl<P = unknown, R = unknown> {
  readonly params: Schema<P>
  readonly result: Schema<R>
  /**
   * The method answers with a stream of chunks, each matching `result`. The provider implements it
   * as an async generator, and a dependent reads it with `stream` rather than with `call`.
   */
  readonly stream?: boolean
  /** How long one call may run, at most 300 seconds. Default: the server's call timeout. */
  readonly timeoutSecs?: number
  /** Shown in interface.d.ts and interface.json. */
  readonly description?: string
}

export type MethodDecls = { readonly [method: string]: MethodDecl }

/** What `defineInterface` takes. */
export interface InterfaceDecl {
  /** Services by name; each name must also be in `provides.services` in plugin.json. */
  readonly services?: { readonly [service: string]: MethodDecls }
  /** Events the plugin publishes: each payload's schema, by topic. */
  readonly events?: { readonly [topic: string]: Schema }
}

/** A declared service, as a host half provides it. */
export interface ServiceDecl<N extends string = string, M extends MethodDecls = MethodDecls> {
  readonly name: N
  readonly methods: M
}

type ServicesOf<D extends InterfaceDecl> = D["services"] extends infer S extends {
  [k: string]: MethodDecls
}
  ? S
  : Record<never, never>

type EventsOf<D extends InterfaceDecl> = D["events"] extends infer E extends {
  [k: string]: Schema
}
  ? E
  : Record<never, never>

/** A declared interface: the default export of a plugin's interface module. */
export interface Interface<D extends InterfaceDecl = InterfaceDecl> {
  readonly __rookery: "interface"
  readonly services: {
    readonly [K in keyof ServicesOf<D> & string]: ServicesOf<D>[K] extends infer M extends MethodDecls
      ? ServiceDecl<K, M>
      : never
  }
  readonly events: EventsOf<D>
}

/** Declares the plugin's services and events. It only builds a description: nothing runs. */
export function defineInterface<const D extends InterfaceDecl>(decl: D): Interface<D> {
  const services: Record<string, ServiceDecl> = {}
  for (const [name, methods] of Object.entries(decl.services ?? {})) {
    if ("stream" in methods) {
      throw new RookeryError(
        "INVALID",
        `${name} has a method called stream, which is how a dependent streams a method; rename it`,
      )
    }
    services[name] = Object.freeze({ name, methods: Object.freeze({ ...methods }) })
  }
  return Object.freeze({
    __rookery: "interface" as const,
    services: Object.freeze(services),
    events: Object.freeze({ ...(decl.events ?? {}) }),
  }) as unknown as Interface<D>
}

export function isInterface(v: unknown): v is Interface {
  return !!v && typeof v === "object" && (v as { __rookery?: unknown }).__rookery === "interface"
}

// ── providing ────────────────────────────────────────────────────────────────────────────────────

type ParamsOf<M> = M extends { readonly params: Schema<infer P> } ? P : never
type ResultOf<M> = M extends { readonly result: Schema<infer R> } ? R : never

/**
 * A method's implementation: it gets the params as its schema parses them, and returns its result.
 * A streaming method returns an async iterable of chunks instead.
 */
export type MethodImpl<M extends MethodDecl> = M extends { readonly stream: true }
  ? (ctx: Ctx, params: ParamsOf<M>) => AsyncIterable<ResultOf<M>> | Promise<AsyncIterable<ResultOf<M>>>
  : (ctx: Ctx, params: ParamsOf<M>) => ResultOf<M> | Promise<ResultOf<M>>

/** Every method of a service, and nothing else. */
export type ServiceImpl<M extends MethodDecls> = { [K in keyof M]: MethodImpl<M[K]> }

/** Rejects members that the service does not declare. */
export type NoExtraMethods<I, M> = { [K in Exclude<keyof I, keyof M>]: never }

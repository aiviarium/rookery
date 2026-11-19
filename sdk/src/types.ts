// Types shared by the host and client halves of the SDK.
import type { Row } from "./data"
import type { RookeryEnv } from "./index"

export interface Disposable {
  dispose(): void
}

export function disposable(fn: () => void): Disposable {
  let done = false
  return {
    dispose() {
      if (done) return
      done = true
      fn()
    },
  }
}

type Env<K extends string, Fallback> = RookeryEnv extends { [P in K]: infer T } ? T : Fallback

/** Settings of the plugin being compiled, from its rookery-env.d.ts. */
export type Settings = Env<"settings", Record<string, unknown>>
export type SettingKey = keyof Settings & string
export type CommandId = Env<"commands", string> | (string & {})
export type ViewId = Env<"views", string> | (string & {})
/** The services of the plugin's required dependencies, from their interface.d.ts, by name. */
export type Services = Env<"services", Record<string, Record<string, { params: any; result: any }>>>
/** The services of its optional dependencies, the same way. */
export type OptionalServices = Env<"optionalServices", Record<never, never>>
/** The plugin's collections, row types by name, from its rookery-env.d.ts. */
export type Collections = Env<"collections", Record<string, Row>>
/** The payload types of the topics the plugin declares, by id, from its rookery-env.d.ts. */
export type TopicPayloads = Env<"topics", Record<string, unknown>>

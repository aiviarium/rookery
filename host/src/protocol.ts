// Messages between the host's main thread and a plugin's worker (postMessage). What the host and the
// server say to each other is described in crates/rookery-plugins/src/exthost.rs.
import type { ErrorBody } from "@rookery/sdk/generated"
import type { Invoke, LogLevel, Registration } from "@rookery/sdk/runtime/host"

export type ToWorker =
  | { type: "activate"; id: number }
  | { type: "invoke"; id: number; invoke: Invoke }
  | { type: "cancel"; callId: string }
  | { type: "notify"; method: string; params: unknown }
  | { type: "deactivate"; id: number }
  | { type: "result"; id: number; result?: unknown; error?: ErrorBody }

export type FromWorker =
  | { type: "activated"; id: number; registrations: Registration[] }
  | { type: "failed"; id: number; error: ErrorBody }
  | { type: "invoked"; id: number; result?: unknown; error?: ErrorBody }
  | { type: "call"; id: number; method: string; params: Record<string, unknown> }
  | { type: "registered"; registration: Registration }
  | { type: "unregistered"; kind: Registration["kind"]; key: string }
  | { type: "log"; level: LogLevel; message: string }
  | { type: "deactivated"; id: number }

/** What a worker is started with. */
export interface WorkerData {
  plugin: string
  dir: string
  manifest: any
}

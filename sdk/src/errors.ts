// The fixed error set. The code survives every hop: a handler throws RookeryError("NOT_FOUND", ...),
// the server passes it on, and clients receive the same code.
import type { ErrorBody, ErrorCode, RpcError } from "./generated"

export type { ErrorCode }

export const ERROR_NUMBERS: Record<ErrorCode, number> = {
  INVALID: -32602,
  FORBIDDEN: -32001,
  NOT_FOUND: -32002,
  CONFLICT: -32003,
  UNAVAILABLE: -32004,
  TIMEOUT: -32005,
  INTERNAL: -32603,
  UNAUTHENTICATED: -32000,
}

const CODES = new Set(Object.keys(ERROR_NUMBERS))

export function isErrorCode(v: unknown): v is ErrorCode {
  return typeof v === "string" && CODES.has(v)
}

/** An error with a code from the fixed set. Throw it from handlers; catch it from calls. */
export class RookeryError extends Error {
  readonly code: ErrorCode
  readonly details?: unknown

  constructor(code: ErrorCode, message: string, details?: unknown) {
    super(message)
    this.name = "RookeryError"
    this.code = code
    this.details = details
  }

  /** The JSON-RPC error object for this error. */
  toRpc(): RpcError {
    const data: { code: ErrorCode; details?: any } = { code: this.code }
    if (this.details !== undefined) data.details = this.details
    return { code: ERROR_NUMBERS[this.code], message: this.message, data }
  }

  toBody(): ErrorBody {
    const b: ErrorBody = { code: this.code, message: this.message }
    if (this.details !== undefined) b.details = this.details as ErrorBody["details"]
    return b
  }

  /** Reads a JSON-RPC error object as it arrives from the server. */
  static fromRpc(e: {
    code?: number
    message?: string
    data?: { code?: string; details?: unknown }
  }): RookeryError {
    const code = isErrorCode(e.data?.code) ? e.data.code : codeForNumber(e.code)
    return new RookeryError(code, e.message ?? code, e.data?.details)
  }

  /** Anything thrown, as a RookeryError. Unknown errors become INTERNAL. */
  static from(e: unknown): RookeryError {
    if (e instanceof RookeryError) return e
    if (e && typeof e === "object") {
      const o = e as { code?: unknown; message?: unknown; details?: unknown; name?: string }
      const message = typeof o.message === "string" ? o.message : String(e)
      if (isErrorCode(o.code)) return new RookeryError(o.code, message, o.details)
      if (o.name === "AbortError" || o.name === "TimeoutError") return new RookeryError("TIMEOUT", message)
      return new RookeryError("INTERNAL", message)
    }
    return new RookeryError("INTERNAL", String(e))
  }
}

export function codeForNumber(n: number | undefined): ErrorCode {
  for (const [code, num] of Object.entries(ERROR_NUMBERS)) if (num === n) return code as ErrorCode
  if (n === -32700 || n === -32600) return "INVALID"
  if (n === -32601) return "NOT_FOUND"
  return "INTERNAL"
}

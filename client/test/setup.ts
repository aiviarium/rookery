// Test preload: registers happy-dom (preserving Bun's native fetch/WebSocket) and the SolidJS transform.

import { GlobalRegistrator } from "@happy-dom/global-registrator"
import "./transform"

const nativeFetch = globalThis.fetch
const nativeRequest = globalThis.Request
const nativeResponse = globalThis.Response
const nativeHeaders = globalThis.Headers
const nativeWebSocket = globalThis.WebSocket

GlobalRegistrator.register({ url: "http://localhost:4790/", width: 1280, height: 800 })

// Happy-DOM replaces fetch/WebSocket with its own Node-HTTP wrappers; restore Bun's native ones
// so tests talking to in-process Bun.serve servers use real HTTP/WebSocket transport cleanly.
globalThis.fetch = nativeFetch
globalThis.Request = nativeRequest
globalThis.Response = nativeResponse
globalThis.Headers = nativeHeaders
globalThis.WebSocket = nativeWebSocket

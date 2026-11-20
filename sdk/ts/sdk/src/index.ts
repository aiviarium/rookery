// Rookery's TypeScript SDK.
//
// A plugin has two halves. Its service runs beside the server on Bun or Node:
// `pluginConfig` reads what the server gives it, `servePlugin` answers on its
// socket, and `cbor` is the encoding of the values that travels as bytes. Its
// client half is a browser module whose entry is `defineClient` from
// `@rookery/sdk/client`.

export { cbor } from "@rookery/client";
export {
  ConfigError,
  type PluginConfig,
  PluginSocketEnv,
  SecretsEnv,
  SocketEnv,
  TokenEnv,
  pluginConfig,
  readSecrets,
} from "./config.ts";
export { type ServedPlugin, type ServeOptions, servePlugin } from "./serve.ts";

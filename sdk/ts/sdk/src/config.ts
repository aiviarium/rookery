// What the server gives a plugin's process: two Unix sockets, a token and,
// when it declared secrets, the file they are in.
//
// The same variables are `pkg/sdk`'s in Go: a plugin written in either language
// reads the same environment.

import { readFileSync } from "node:fs";

/** SocketEnv names the server's Unix socket, which the plugin calls. */
export const SocketEnv = "ROOKERY_SOCKET";

/** PluginSocketEnv names the Unix socket the plugin serves on. */
export const PluginSocketEnv = "ROOKERY_PLUGIN_SOCKET";

/** TokenEnv names the credential the plugin presents on the server's socket. */
export const TokenEnv = "ROOKERY_PLUGIN_TOKEN";

/** SecretsEnv names the file a provisioner's secrets are in, when it has any. */
export const SecretsEnv = "ROOKERY_SECRETS";

/** ConfigError is what a plugin's environment is refused with. */
export class ConfigError extends Error {
  override readonly name = "ConfigError";
}

/** PluginConfig is what the server gives a plugin process. */
export interface PluginConfig {
  /** socket is the server's Unix socket, `ROOKERY_SOCKET`. */
  readonly socket: string;
  /** pluginSocket is the socket this process serves on, `ROOKERY_PLUGIN_SOCKET`. */
  readonly pluginSocket: string;
  /** token is the credential for the server's socket, `ROOKERY_PLUGIN_TOKEN`. */
  readonly token: string;
  /** secretsFile is where the declared secrets' values are, empty when none. */
  readonly secretsFile: string;
}

/**
 * pluginConfig reads the configuration of a plugin process.
 *
 * It throws a `ConfigError` naming the variable that is missing, rather than
 * serving on a socket nobody named, which a service started by hand would.
 */
export function pluginConfig(env: Record<string, string | undefined> = process.env): PluginConfig {
  const config: PluginConfig = {
    socket: value(env, SocketEnv),
    pluginSocket: value(env, PluginSocketEnv),
    token: value(env, TokenEnv),
    secretsFile: value(env, SecretsEnv),
  };
  for (const [name, field] of [
    [SocketEnv, config.socket],
    [PluginSocketEnv, config.pluginSocket],
    [TokenEnv, config.token],
  ] as const) {
    if (field === "") {
      throw new ConfigError(`sdk: ${name} is not set: a plugin reads it from the server's environment`);
    }
  }
  return config;
}

/**
 * readSecrets reads the values of the secrets a provisioner declared, from the
 * file `ROOKERY_SECRETS` names. A value is never in the environment or on a
 * command line, so it is read from the file and nowhere else.
 */
export function readSecrets(file: string): Readonly<Record<string, string>> {
  if (file === "") {
    return {};
  }
  let text: string;
  try {
    text = readFileSync(file, "utf8");
  } catch (error) {
    throw new ConfigError(`sdk: reading ${SecretsEnv}: ${(error as Error).message}`);
  }
  const values: unknown = JSON.parse(text);
  if (values === null || typeof values !== "object" || Array.isArray(values)) {
    throw new ConfigError(`sdk: ${SecretsEnv} is not a JSON object of name to value`);
  }
  const secrets: Record<string, string> = {};
  for (const [name, value] of Object.entries(values as Record<string, unknown>)) {
    if (typeof value !== "string") {
      throw new ConfigError(`sdk: the secret ${name} in ${SecretsEnv} is not a string`);
    }
    secrets[name] = value;
  }
  return secrets;
}

/** value is an environment variable, or the empty string when it is unset. */
function value(env: Record<string, string | undefined>, name: string): string {
  return (env[name] ?? "").trim();
}

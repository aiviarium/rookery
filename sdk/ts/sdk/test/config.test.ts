import { afterAll, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  ConfigError,
  PluginSocketEnv,
  SecretsEnv,
  SocketEnv,
  TokenEnv,
  pluginConfig,
  readSecrets,
} from "../src/config.ts";

const env = {
  [SocketEnv]: "/run/rookery/server.sock",
  [PluginSocketEnv]: "/run/rookery/plugin.sock",
  [TokenEnv]: "example-token",
};

describe("pluginConfig", () => {
  test("reads the server's environment", () => {
    const config = pluginConfig({ ...env, [SecretsEnv]: "/run/rookery/secrets.json" });
    expect(config).toEqual({
      socket: "/run/rookery/server.sock",
      pluginSocket: "/run/rookery/plugin.sock",
      token: "example-token",
      secretsFile: "/run/rookery/secrets.json",
    });
  });

  test("has no secrets file when the plugin declared none", () => {
    expect(pluginConfig(env).secretsFile).toBe("");
  });

  test("names the variable a plugin is missing", () => {
    const missing = { ...env };
    delete (missing as Record<string, string | undefined>)[TokenEnv];
    expect(() => pluginConfig(missing)).toThrow(ConfigError);
    expect(() => pluginConfig(missing)).toThrow(TokenEnv);
  });

  test("refuses a blank socket rather than serving nowhere", () => {
    expect(() => pluginConfig({ ...env, [PluginSocketEnv]: "  " })).toThrow(PluginSocketEnv);
  });
});

describe("readSecrets", () => {
  const dir = mkdtempSync(join(tmpdir(), "rookery-sdk-"));
  afterAll(() => rmSync(dir, { recursive: true, force: true }));

  test("reads a JSON object of name to value", () => {
    const file = join(dir, "secrets.json");
    writeFileSync(file, JSON.stringify({ example_token: "s3cret" }));
    expect(statSync(file).isFile()).toBe(true);
    expect(readSecrets(file)).toEqual({ example_token: "s3cret" });
  });

  test("has nothing for a plugin without secrets", () => {
    expect(readSecrets("")).toEqual({});
  });

  test("refuses a value that is not a string", () => {
    const file = join(dir, "numbers.json");
    writeFileSync(file, JSON.stringify({ example_token: 7 }));
    expect(() => readSecrets(file)).toThrow(ConfigError);
  });

  test("refuses a file that is not there", () => {
    expect(() => readSecrets(join(dir, "absent.json"))).toThrow(ConfigError);
  });
});

import { describe, expect, test } from "bun:test";

import { cbor } from "../src/cbor.ts";

/** hex is a CBOR encoding, for the assertions that care about its exact bytes. */
function hex(bytes: Uint8Array): string {
  return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

describe("cbor", () => {
  test("writes a map with its shortest length", () => {
    expect(hex(cbor.encode({ a: 1 }))).toBe("a1616101");
  });

  test("writes a byte string, not a typed array's tag", () => {
    expect(hex(cbor.encode({ b: new Uint8Array([1, 2]) }))).toBe("a16162420102");
  });

  test("writes a date as tag 0 text, which internal/cborx reads", () => {
    const bytes = cbor.encode({ at: new Date("2026-11-20T10:00:00.500Z") });
    // a1 "at" tag 0 text(24) 2026-11-20T10:00:00.500Z
    expect(hex(bytes).slice(0, 12)).toBe("a1626174c078");
    const decoded = cbor.decode<{ at: Date }>(bytes);
    expect(decoded?.at.toISOString()).toBe("2026-11-20T10:00:00.500Z");
  });

  test("round-trips the values a record holds", () => {
    const value = {
      title: "A note",
      done: false,
      count: 3,
      score: 1.5,
      tags: ["a", "b"],
      about: { id: "01hother" },
      body: new Uint8Array([0x2a]),
      nothing: null,
    };
    const decoded = cbor.decode<typeof value>(cbor.encode(value));
    expect(decoded).toEqual(value);
  });

  test("decodes the tag 0 text a Go writer produces", () => {
    const text = "2026-11-20T10:00:00.500Z";
    const bytes = new Uint8Array([0xa1, 0x62, 0x61, 0x74, 0xc0, 0x78, text.length, ...Buffer.from(text)]);
    const decoded = cbor.decode<{ at: Date }>(bytes);
    expect(decoded?.at.toISOString()).toBe(text);
  });

  test("treats empty bytes as no value", () => {
    expect(cbor.decode(undefined)).toBeUndefined();
    expect(cbor.decode(new Uint8Array())).toBeUndefined();
  });

  test("refuses a value that contains itself", () => {
    const value: Record<string, unknown> = {};
    value["self"] = value;
    expect(() => cbor.encode(value)).toThrow(TypeError);
  });
});

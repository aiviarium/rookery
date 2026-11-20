// CBOR for the dynamic values in Rookery's `*_cbor` fields: datastore records,
// filters, job arguments, settings values, payloads. Go writes them through
// `internal/cborx`, which is Core Deterministic CBOR (RFC 8949 4.2.1), and so
// does this: maps are written with their shortest length, keys as given and
// dates as tag 0 text in RFC 3339, in UTC.
import { Encoder, Tag } from "cbor-x";

/**
 * cbor is the encoder for Rookery's dynamic values.
 *
 * `cbor-x`'s records extension, its typed-array tag and its fixed-size map
 * headers are off, so a map is a CBOR map with its shortest length and a
 * `Uint8Array` is a byte string, which is what Go's `internal/cborx` reads. A
 * `Date` is written as tag 0 text; JavaScript dates keep milliseconds only, so a
 * time that needs nanoseconds travels as an RFC 3339 string.
 */
const encoder = new Encoder({
  useRecords: false,
  mapsAsObjects: true,
  variableMapSize: true,
  structuredClone: false,
  tagUint8Array: false,
});

/** cborKit is the encode and decode pair, and the helpers around it. */
export const cbor = {
  /** encode returns the CBOR of a value. */
  encode(value: unknown): Uint8Array {
    return encoder.encode(tagged(value, new Set())) as Uint8Array;
  },

  /** decode reads CBOR, or returns undefined for an absent field's empty bytes. */
  decode<T = unknown>(bytes: Uint8Array | undefined | null): T | undefined {
    if (bytes === undefined || bytes === null || bytes.length === 0) {
      return undefined;
    }
    return encoder.decode(bytes) as T;
  },

  /**
   * time returns a Date as the tag 0 text Go writes: RFC 3339 with
   * milliseconds, in UTC. A value with more precision travels as a string.
   */
  time(date: Date): Tag {
    return new Tag(date.toISOString(), 0);
  },
};

/** tagged rewrites dates as tag 0 text, which is how `internal/cborx` writes them. */
function tagged(value: unknown, seen: Set<object>): unknown {
  if (value instanceof Date) {
    return new Tag(value.toISOString(), 0);
  }
  if (value === null || typeof value !== "object") {
    return value;
  }
  if (seen.has(value)) {
    throw new TypeError("cbor: a value cannot contain itself");
  }
  seen.add(value);
  try {
    if (Array.isArray(value)) {
      return value.map((item) => tagged(item, seen));
    }
    if (value instanceof Map) {
      const map = new Map<unknown, unknown>();
      for (const [key, item] of value) map.set(key, tagged(item, seen));
      return map;
    }
    if (value instanceof Uint8Array || value instanceof ArrayBuffer) {
      return value;
    }
    const object: Record<string, unknown> = {};
    for (const [key, item] of Object.entries(value)) {
      if (item === undefined) continue;
      object[key] = tagged(item, seen);
    }
    return object;
  } finally {
    seen.delete(value);
  }
}

# Filters (`where`)

The structured filter of `find`, `count` and live queries. The Rust crate
`rookery-filter` parses, validates and matches rows in memory; `rookery-store` adapts collection fields
and compiles the validated filter into SQL. `@rookery/sdk`'s `filter.ts` evaluates the same filter and
orders rows in memory.

The store's integration test `crates/rookery-store/tests/queries.rs`
(`where_operators_and_in_memory_matching_agree`) runs each reference case in SQL and in memory
and records the collection, rows, refused filters and orders in `vectors/filter.json` here.
The standalone Rust tests (`tests/vectors.rs`) and the SDK's tests
replay that same recording without a database.

## Shape

```ts
type Where = { [key: string]: Condition | Where[] | Where }

// key "and": Where[]   all must match (an empty list matches everything)
// key "or":  Where[]   at least one must match (an empty list matches nothing)
// key "not": Where     must not match
// any other key: a field name, and its value is a Condition

type Condition =
  | Scalar                                  // shorthand for { eq: value }
  | { eq?: Value; ne?: Value
      in?: Value[]; nin?: Value[]
      lt?: Value; lte?: Value; gt?: Value; gte?: Value
      contains?: string | string[] }

type Scalar = string | number | boolean | null
```

- Several keys in one object are combined with AND, and so are several operators in one
  condition: `{ estimate: { gte: 1, lt: 8 }, state: "open" }`.
- **A condition that is an object is always an operator object.** Equality with an object or a
  list (only meaningful for `json` fields) is written `{ eq: {...} }` / `{ eq: [...] }`. A bare
  list is an error (use `in`).
- Unknown fields and unknown operators are errors (`INVALID`), not "no match".
- Limits: at most 256 keys in total, nesting depth 16, 1000 values in `in`/`nin`.

`and`, `or` and `not` are reserved and cannot be field names.

## Fields

Every field of the collection can be filtered, plus these system fields, named as they appear
in rows:

| Field | Type |
| --- | --- |
| `id` | id (UUID) |
| `spaceId` | id (space-scoped collections) |
| `userId` | id (user-scoped collections) |
| `createdAt`, `updatedAt` | datetime |
| `deletedAt` | datetime (collections with `trash`) |
| `createdBy`, `updatedBy` | id |
| `rev` | integer |

## Values

Filter values are validated and normalised by field type, exactly like values on write:

| Type | Value | Compared as |
| --- | --- | --- |
| `text`, `longtext`, `path` | string | by Unicode code point (byte order of UTF-8) |
| `enum` | one of the declared values | by code point |
| `integer`, `sequence`, `number` | number (`integer`: no fraction) | numerically |
| `boolean` | boolean | equality only |
| `date` | `"YYYY-MM-DD"` | as a date |
| `datetime` | RFC 3339 string, any offset | as an instant (`"2025-12-24T08:30:00+02:00"` equals `"2025-12-24T06:30:00Z"`) |
| id | UUID string | as a UUID (case-insensitive) |
| `ref` | entity URI; with `to`, a bare id is turned into `<to>://<id>` | as the normalised URI string |
| `refs` | `contains` only, a URI or a list of URIs, normalised like `ref` | |
| `json` | any JSON, with `eq`/`ne` | JSON equality: numbers by value (`1` equals `1.0`), object key order ignored |
| `richtext` | `eq`/`ne` with a document, as `json` | |

Rows returned by the store write datetimes as `2025-12-24T06:30:00+00:00` (UTC, microseconds
when present) and numbers as JSON numbers; an implementation must parse, not compare strings,
for `datetime`.

## Operators

Evaluation is two-valued: a condition is either true or false, never unknown. `v` is the row's
value for the field (a missing field counts as `null`), `x` the filter value.

| Operator | True when |
| --- | --- |
| `eq: null` | `v` is null |
| `eq: x` | `v` is not null and `v = x` |
| `ne: null` | `v` is not null |
| `ne: x` | `v` is null or `v ≠ x` (nulls count as different) |
| `in: [x…]` | `v` equals some non-null `x`, or `v` is null and the list contains `null` |
| `nin: [x…]` | not `in` (so a null `v` matches `nin` unless the list contains `null`) |
| `lt`, `lte`, `gt`, `gte` | `v` is not null and compares accordingly; `x` may not be null. Not for `boolean`, `json`, `richtext`, `refs` |
| `contains: x` or `[x…]` | `v` is a list containing every `x` (`refs` fields only) |
| `not: W` | `W` is false |

Text ordering is by code point everywhere (the SQL uses `COLLATE "C"` for these comparisons), so
`"Delta" < "alpha"`.

## Not part of the filter

- `search` (full-text) is a separate query option. A live query that uses it cannot be
  evaluated on the client and falls back to a refetch.
- Ordering (`orderBy`) does not affect matching. Note that ordering uses the database's
  collation for text, which is not code-point order.

`bytes` and `vector` fields accept only `eq: null` (including the shorthand null) and `ne: null`. They cannot be ordered or used with `in`, comparisons or `contains`. A `blob` id uses ordinary text comparison. Both ports run `vectors/binary.json` for these rules.

**Log collections** (`kind: "log"`) have the system fields `id`, `seq` (integer), `createdAt`, `createdBy` and the scope field (`spaceId` or `userId`); `rev`, `updatedAt`, `updatedBy` and `deletedAt` do not exist there, so a filter naming them is refused. Both ports run `vectors/log.json` for this, and the store runs the same cases on Postgres.

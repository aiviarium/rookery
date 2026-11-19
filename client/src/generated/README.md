# Generated wire types

This folder holds the TypeScript wire types for Rookery's JSON-RPC and stream protocol.

`crates/rookery-proto/tests/bindings.rs` writes the types of `rookery-proto` here and the `index.ts`
that exports them. Run it again rather than editing a file it writes:

```bash
ROOKERY_UPDATE_DOCS=1 cargo test -p rookery-proto --features ts --test bindings
```

The bindings test owns the files it writes and `index.ts`; it leaves other files alone. The rest are
the prototype's, ported ahead of `rookery-proto` and kept for the milestones of `plans/v1.md` that
bring them: the data layer (`Data*`, `Change`, `LiveQuery`), SQL and documents (`Sql*`, `Document*`),
env and terminals (`Env*`, `Terminal*`), spaces, the plugin system (`Plugin*`, `Manifest`) and the
extension host. A type no method of `rookery serve` returns yet describes what those will send, not
what the server sends now.

`sdk/src/wire.ts` is where the client writes down which methods it calls and their generated types;
`sdk/test/wire.test.ts` checks that list against `api/methods.json`.

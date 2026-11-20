# Working in rookery

Rookery runs a team's machines, their desktops, and the tools built on them. [PLAN.md](PLAN.md) is the source of truth: what the system is, its layout, its libraries, its conventions and the order the code is written in. Read it before a change of any size. This file is the short version.

## Layout

One Go workspace. `cmd/` holds only `main` packages, `internal/` the packages that do the work, `proto/` the contract and `gen/go/` buf's output for it. Plugins are their own modules under `plugins/`. Packages are named for what they do; none is `util`, `common` or `types`.

## Libraries

Add only what [PLAN.md](PLAN.md) lists under "Libraries". Anything else needs a yes in the pull request.

## Conventions

- The latest stable Go in `go.mod`. `gofmt`, `go vet` and `golangci-lint` are clean; `make lint` runs all three.
- Every function that does I/O or can block takes `context.Context` first.
- Errors are wrapped with `%w`. Domain errors are typed or sentinel, and checked with `errors.Is`/`errors.As`. `access.ToConnect` is the only place that maps them to connect codes; a capability refusal carries `rookery.v1.Refusal` as its detail.
- No package-level mutable state. Dependencies go in through constructors; `internal/server` wires everything in one place.
- Every goroutine has an owner that cancels and waits for it (`errgroup`). Tests run with `-race`.
- `log/slog` with attributes. Secrets never reach a log, an error, argv or a child's environment: child processes get them in files of mode 0600 or on stdin.
- Configuration is `ROOKERY_*` variables and flags, read once into a config struct per program.
- Generated code is checked in, never edited. CI fails while it is stale.
- Tests are table-driven where cases differ in data, call `t.Parallel()`, use `t.TempDir()`, and put Unix sockets in a short directory on disk under `TMPDIR` (such as `~/.tmp`), never `/tmp`.
- Tests that need Postgres use `internal/store/storetest` and `ROOKERY_TEST_POSTGRES`. They skip when it is unset, and fail when `CI` is set.
- Queries live in sqlc except the datastore's, which builds them with parameters only.
- Before the first tagged release the core schema is one file, `internal/store/schema.sql`, with its digest in `rookery.meta`. There are no migrations: a change edits the file, and a database with another digest is reset with `rookery server reset`.

## Commands

```bash
make tools    # install the pinned buf, sqlc, protoc plugins and golangci-lint
make gen      # buf generate, sqlc generate
make db       # Postgres with pgvector on 127.0.0.1:55432, fsync off
make reset    # drop and recreate the dev database's schemas
make test     # go test -race in every module
make lint     # gofmt, go vet, golangci-lint, buf lint, ansible-lint
make ts       # bun install, typecheck and test in sdk/ts
make e2e      # go test -tags e2e ./e2e/...
make dev      # the dev database and rookery serve against it
```

## Commits

Subjects are short and specific. A commit written with an AI's help says so with one trailer per model, such as `Assisted-by: OpenCode:deepseek-v4.1-flash`, and nothing else.

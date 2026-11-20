# Rookery core in Go

This replaces `design.md`, `v1.md` and `gaps.md` for Rookery. It describes one Go repository, the system it builds, the order the code is written in, and how the two agents that write it work. Nothing from the Rust Rookery, Hatchery or the prototype is kept. They can be read for behaviour, never copied.

## What it is

Rookery runs a team's machines, their desktops, and the tools built on them. It has two halves in one server:

- **Infrastructure.** Hosts and their profiles, reached over a tailnet and SSH, set up with Ansible, created through provisioners (Incus and Docker come with Rookery), rolled out in batches, used through desktops, terminals, exec, files and ports, with `box` on each host for the programs running there. Every change is a job with a log.
- **The framework.** People, groups, spaces and grants; a datastore with scoping, references, history, a change feed, live queries and search; live documents; jobs with schedules and webhooks; events, topics, leases and notifications; settings, secrets, connections and blobs; a plugin system with services in any language, client halves in the browser, and extension points on hosts; SDKs.

Plugins build products on it. A UI such as Crow is a shell plugin and a set of plugins, served by Rookery and talking to its edge. Rookery ships no UI of its own.

The repository is `aiviarium/rookery`, on a new orphan branch `go`. It replaces `main` when the end-to-end test below passes. The Rust history stays reachable on the tag `rust-final`.

### Not in the core

- Agents and Parrot. Nothing in Rookery starts, drives or knows about coding agents. A plugin can add them later through host features, host services and `box` extensions.
- Porting the other existing plugins (`backup`, `github`, `unity`, and Crow's `fleet`, `terminal`, `ai`, `projects`, `docs`, `dashboards`, `workbench`, `code`). They are rebuilt on this core later.
- USB and ISO installers, QEMU boots, physical laptops as hosts, the connector and the desktop app.
- Compatibility of any kind with the Rust Rookery, and importing its store.

### No migrations before a tagged release

Until the first tagged release nothing is migrated:

- The core schema is one file, `internal/store/schema.sql`. A change edits it. The server stores the file's SHA-256 in `rookery.meta`; on a database with another digest it refuses to start and says to run `rookery server reset`, which drops and recreates the schemas after a confirmation.
- A plugin update whose collection declarations differ from the installed ones is refused unless `--reset-data` is given, which drops and recreates that plugin's tables.
- Protos, the manifest format, the CLI and the client configuration change freely. `buf breaking` is not run.

At the first tagged release, the schema at that tag becomes migration 1, and numbered migrations start from there.

## Wire

- Every service is defined in protobuf under `proto/` and served with [connect-go](https://connectrpc.com).
- Between Rookery's own processes (server, `rookeryd`, `box`, plugin processes) the protocol is gRPC only. Internal Go clients use `connect.WithGRPC()`. Plugins in other languages use their language's standard gRPC library.
- The edge is the server's public listener. It serves the same handlers to the CLI as gRPC and to browsers as Connect and gRPC-Web. Only the edge adds plain HTTP and WebSocket routes, listed below.
- Typed fields are protobuf fields. Dynamic content (datastore records, feature variables, provisioner arguments and state, action and job arguments, settings values, kv values, channel, event and topic payloads, host service calls) is a `bytes` field holding CBOR, named `*_cbor`. Go encodes it through `internal/cborx` in Core Deterministic mode. The SDKs use `fxamacker/cbor` in Go and `cbor-x` in TypeScript.
- Postgres stores opaque dynamic content as `bytea` CBOR. Datastore fields are real columns.

### Process links

| Link | Transport |
| --- | --- |
| Server → `rookeryd` | An SSH session running `sudo -n rookeryd connect`; its stdio becomes a `net.Conn` through `internal/pipeconn`, with HTTP/2 cleartext (`x/net/http2`) and connect-go on top |
| `box` and other local programs → `rookeryd` | gRPC on `rookeryd`'s user socket, with the caller's uid from `SO_PEERCRED` |
| Server → a plugin process (service, provisioner) | gRPC on a Unix socket the server names in `ROOKERY_PLUGIN_SOCKET`, which the process listens on |
| A plugin process → the server | gRPC on the server's Unix socket (`ROOKERY_SOCKET`), with `ROOKERY_PLUGIN_TOKEN` as its credential |

## Repository

A Go workspace (`go.work`) with the core module and one module per first-party plugin, so the plugins' dependencies (the Docker and Incus clients) stay out of the core's graph.

```
rookery/
├─ go.work
├─ go.mod                       module github.com/aiviarium/rookery
├─ cmd/
│  ├─ rookery/                  CLI and `rookery serve`
│  ├─ rookeryd/                 host daemon
│  └─ box/                      the host-side tool for users and programs on the host
├─ proto/rookery/
│  ├─ v1/                       the edge API, one file per service
│  ├─ host/v1/                  server ↔ rookeryd, and box ↔ rookeryd
│  └─ plugin/v1/                server ↔ plugin processes, and ServerService for plugins
├─ gen/go/                      buf output for Go, checked in
├─ internal/
│  ├─ server/                   config, listeners, leader lock, start-up, shutdown, wiring
│  ├─ edge/                     HTTP mux, auth middleware, WebSockets, proxies, static assets, webhooks
│  ├─ authn/                    users, links, device sessions, tokens, OIDC, groups from claims
│  ├─ access/                   levels, resources, grants, inheritance, the interceptor
│  ├─ spaces/
│  ├─ store/                    pgx pool, schema.sql, sqlc queries, reset, storetest
│  ├─ seal/                     sealing and rekey
│  ├─ data/                     the datastore
│  ├─ filter/                   the filter language
│  ├─ entities/                 entity registry and search
│  ├─ docs/                     live documents
│  ├─ jobs/                     records, claims, runner, workers, logs, retries, schedules
│  ├─ rollouts/                 roll-outs over hosts
│  ├─ messaging/                events, topics, leases, notifications
│  ├─ blobs/
│  ├─ config/                   settings, secrets, connections
│  ├─ hosts/                    records, profiles, labels, types, capability checks, channel take-in
│  ├─ desktop/                  the server side of desktops: proxy, screenshots, input
│  ├─ tailnet/                  tsnet, the control API, devices and keys
│  ├─ link/                     SSH client, key ring, known hosts, links to rookeryd
│  ├─ apply/                    Ansible work directory, inventory, playbook, run
│  ├─ provision/                provisioner entries, resources, machines, seeds
│  ├─ plugins/                  manifest, packages, registry, install, supervision, ServerService
│  ├─ agent/                    rookeryd: sessions, exec, files, ports, channel, host services, desktop relay
│  ├─ boxcli/                   box's commands
│  ├─ x11/                      screenshots and input for the desktop, over X11 with XTest
│  ├─ cli/                      rookery's commands, output, client config
│  ├─ cborx/
│  └─ pipeconn/
├─ pkg/sdk/                     the Go SDK for plugins
├─ plugins/                     first-party plugins, each its own Go module, bundled with releases
│  ├─ docker/
│  └─ incus/
├─ sdk/
│  ├─ ts/                       bun workspace: @rookery/proto, @rookery/client, @rookery/sdk
│  ├─ python/, jvm/, dotnet/, rust/
│  └─ conformance/
├─ ansible/                     go:embed: the base and desktop roles, the playbook template
├─ images/                      Dockerfiles: the server, the machine image for Docker, the test host
├─ testdata/plugins/            fixture plugins
├─ e2e/                         the end-to-end test, build tag e2e
├─ dev/                         compose.yaml for Postgres and test hosts
├─ buf.yaml, buf.gen.yaml, sqlc.yaml, .golangci.yml, Makefile
└─ AGENTS.md
```

`cmd/` holds only `main` packages. Packages are named for what they do; none is called `util`, `common` or `types`. Interfaces are declared where they are used.

## Libraries

Adding a dependency not on this list needs the orchestrator's yes in the PR.

| Need | Library |
| --- | --- |
| RPC | `connectrpc.com/connect`, `google.golang.org/protobuf`, `golang.org/x/net/http2`, `connectrpc.com/grpcreflect` |
| Generation | `buf` with `protoc-gen-go`, `protoc-gen-connect-go`, `protoc-gen-es`; `sqlc` |
| Postgres | `github.com/jackc/pgx/v5`, `github.com/pgvector/pgvector-go` |
| CBOR | `github.com/fxamacker/cbor/v2` |
| SSH | `golang.org/x/crypto/ssh`, `golang.org/x/crypto/ssh/knownhosts` |
| Tailnet | `tailscale.com/tsnet`, `tailscale.com/client/tailscale/v2`; Headscale's API through a small client in `internal/tailnet` |
| Sealing | `golang.org/x/crypto/chacha20poly1305` |
| OIDC | `github.com/coreos/go-oidc/v3`, `golang.org/x/oauth2` |
| WebSocket | `github.com/coder/websocket` |
| PTY | `github.com/creack/pty` |
| X11 | `github.com/jezek/xgb` (with its `xtest` extension), pure Go |
| Host channel store | `go.etcd.io/bbolt` |
| Schedules | `github.com/robfig/cron/v3`, the parser only |
| Blobs on S3 | `github.com/minio/minio-go/v7` |
| CLI | `github.com/spf13/cobra` |
| Logging, tests | `log/slog`; `testing`, `github.com/google/go-cmp` |
| Docker plugin | `github.com/docker/docker/client`, `github.com/compose-spec/compose-go/v2` |
| Incus plugin | `github.com/lxc/incus/v6/client` |
| TypeScript SDK | `@connectrpc/connect`, `@connectrpc/connect-web`, `@bufbuild/protobuf`, `cbor-x`, `solid-js@^2` (peer, for `@rookery/client/solid` only) |

No ORM, no dependency-injection framework, no assertion library, no mock generator.

## Go conventions

- The latest stable Go in `go.mod`. `gofmt`, `go vet` and `golangci-lint` (`errcheck`, `staticcheck`, `revive`, `gosec`, `errorlint`, `contextcheck`, `noctx`, `bodyclose`) are clean.
- Every function that does I/O or can block takes `context.Context` first.
- Errors are wrapped with `%w` and context. Domain errors are typed or sentinel and checked with `errors.Is`/`errors.As`. One function, `access.ToConnect`, maps them to connect codes: `InvalidArgument`, `Unauthenticated`, `PermissionDenied`, `NotFound`, `Aborted` (revision or lease conflict), `Unavailable` (host offline, capability false, standby), `DeadlineExceeded`, `Internal`. A capability refusal carries `rookery.v1.Refusal` as an error detail.
- No package-level mutable state. Dependencies go in through constructors; `internal/server` wires everything in one place.
- Every goroutine has an owner that cancels and waits for it (`errgroup`). Tests run with `-race`.
- `slog` with attributes. Secret values never reach a log, an error, argv or another process's environment. Child processes get them in files of mode 0600 or on stdin.
- Configuration is `ROOKERY_*` variables and flags, read once into a config struct per program.
- Generated code is checked in, never edited, and CI fails while it is stale.
- Tests are table-driven where cases differ in data, call `t.Parallel()`, use `t.TempDir()`, and put Unix sockets in a short directory under a `TMPDIR` on disk (such as `~/.tmp`), never `/tmp`.
- Queries live in sqlc, except the datastore's, which `internal/data` builds with parameters only. Identifiers it writes come from validated manifests and are quoted with `pgx.Identifier`.

## The server

### Start-up and standby

1. Read the config; open the pool to `ROOKERY_DATABASE_URL`.
2. Try `pg_advisory_lock` on a dedicated connection (`ROOKERY_DATABASE_LOCK_URL` when the main URL goes through a pooler). While another server leads, this one is a standby: it serves `/healthz` with 503 and waits on the lock.
3. As leader: apply `schema.sql` to an empty database or check its digest, load the sealing key (`ROOKERY_SECRET_KEY_FILE` or `ROOKERY_SECRET_KEY`, 64 hex digits; created at the file's path on an empty database), fail jobs left running as `interrupted` and pause roll-outs that were running, start `tsnet`, install the bundled plugins that aren't installed, write the installed plugins' folders from the database, start plugin services, job workers, the scheduler, the live feed and the host channel followers.
4. Listen on the edge (`ROOKERY_LISTEN`, TLS when a certificate is configured, h2c otherwise; also on the tailnet when `ROOKERY_TAILNET_LISTEN` is set) and on the Unix socket (`ROOKERY_SOCKET`, default `/run/rookery/rookery.sock`, mode 0660).
5. On SIGTERM, or when the lock connection drops: stop accepting calls, cancel jobs (they end `interrupted`), stop plugin processes, close links and `tsnet`, exit. A process manager restarts it as a standby.

### Edge routes

| Route | For |
| --- | --- |
| `/rookery.v1.*/` | Every core service, as gRPC, Connect and gRPC-Web |
| `/healthz` | 200 on the leader, 503 on a standby |
| `/auth/link/{token}`, `/auth/oidc/start`, `/auth/oidc/callback`, `/auth/cli`, `/auth/logout` | Browser and CLI sign-in |
| `/ws/terminals/{id}` | A browser attached to a terminal |
| `/ws/hosts/{name}/desktop` | The host's desktop as RFB over WebSocket, for noVNC in a client half |
| `/ws/hosts/{name}/ports/{port}` | Raw bytes to a port on a host's loopback |
| `/hosts/{name}/ports/{port}/…` | HTTP proxied to a port on a host's loopback |
| `/ws/documents/{id}` | A browser syncing a live document |
| `/hooks/{plugin}/{name}` | Incoming webhooks, verified as the plugin declared, enqueued as jobs |
| `/blobs/{id}` | Reading a blob the caller may read, with range requests |
| `/plugins/{id}/{hash}/…` | A plugin's client half and assets, immutable, cacheable |
| `/plugins/{id}/rpc/…` | A plugin service's own RPCs, proxied with access checks |
| `/` and every other path | The installed shell plugin's `index.html`, or a plain page saying no shell is installed |

## Edge API, `proto/rookery/v1`

Each RPC declares its access with a method option, `option (rookery.v1.access) = { level: LEVEL_MANAGE, resource: "host" };`, naming the request field that holds the resource. The access interceptor resolves the caller, reads the option, and refuses before the handler runs. A test walks every registered procedure, core and plugin, and fails for one without an access option.

| Service | RPCs |
| --- | --- |
| `AuthService` | `Me`, `ListSessions`, `RevokeSession`, `CreateToken`, `ListTokens`, `RevokeToken` |
| `UserService` | `List`, `Search`, `CreateLink`, `SetAdmin`, `SetDisabled` |
| `GroupService` | `List`, `Create`, `Rename`, `Delete`, `AddMember`, `RemoveMember`, `BindClaim` |
| `AccessService` | `Check`, `Level`, `ListGrants`, `Grant`, `Revoke` |
| `SpaceService` | `ListKinds`, `List`, `Get`, `Create`, `Rename`, `Archive`, `Delete` |
| `DataService` | `Get`, `Find`, `Count`, `Aggregate`, `Insert`, `Update`, `Upsert`, `Delete`, `Restore`, `Batch`, `History`, `RefsTo`, `RefsFrom`, `Live` (server stream), `Changes` (server stream from a cursor) |
| `EntityService` | `Resolve`, `ResolveMany`, `Search` |
| `DocumentService` | `Create`, `Get`, `Sync` (bidi), `Snapshot`, `History`, `Delete` |
| `JobService` | `Enqueue`, `List`, `Get`, `Follow` (server stream), `Cancel`, `Retry` |
| `ScheduleService` | `List`, `Create`, `Update`, `Delete`, `RunNow` |
| `RolloutService` | `Plan`, `Start`, `List`, `Get`, `Watch` (server stream), `Pause`, `Resume`, `Cancel` |
| `EventService` | `Subscribe` (server stream) |
| `TopicService` | `Publish`, `Subscribe` (server stream), `Ack`, `Nack`, `Inspect`, `Retry`, `Discard` |
| `NotificationService` | `List`, `Watch` (server stream), `MarkRead`, `Answer`, `SetPreference` |
| `SettingService` | `List`, `Get`, `Set`, `Reset` |
| `SecretService` | `List`, `Set`, `Delete`, `Rename` |
| `ConnectionService` | `ListTypes`, `List`, `Get`, `Create`, `Update`, `Delete`, `Test` |
| `BlobService` | `Put` (client stream), `Get`, `Delete` |
| `AuditService` | `List` |
| `HostService` | `List`, `Get`, `Watch`, `Add`, `Create`, `Remove`, `Rename`, `SetProfile`, `SetLabels`, `Apply`, `ListActions`, `RunAction`, `Check` |
| `DesktopService` | `Screenshot`, `Input`, `GetClipboard`, `SetClipboard`, `Pause`, `Resume`, `Connect` (bidi RFB bytes, for the CLI) |
| `TerminalService` | `Open`, `List`, `Attach` (bidi), `Resize`, `Close` |
| `ExecService` | `Run` (server stream) |
| `FileService` | `Read`, `Write`, `List`, `Stat`, `Delete` |
| `PortService` | `Connect` (bidi bytes to a port or Unix socket on the host) |
| `ChannelService` | `Publish` (to a host), `Read` (a topic's kept messages) |
| `HostCallService` | `List` (the host services registered on a host), `Call` |
| `KeyService` | `List`, `Rotate`, `ListKnownHosts`, `TrustHostKey`, `ForgetHostKey` |
| `TailnetService` | `Status`, `Configure`, `ListDevices` |
| `ProvisionerService` | `ListTypes`, `List`, `Get`, `Add`, `Update`, `Remove`, `Status`, `ListResources`, `RunAction` |
| `PluginService` | `List`, `Get`, `Install` (client stream or registry reference), `Update`, `Remove`, `SetEnabled`, `Check`, `Contributions` |
| `LocalService` | `SetupLink`, `CreateToken`, `Rekey`, `Reset`, `Dial` (bidi bytes through the tailnet, for Ansible's SSH). On the Unix socket only |

Long-running RPCs return a job or roll-out id at once; the caller follows it.

## People and access

- **Users** have an id, email, display name, `admin` flag and `disabled` flag.
- **Sign-in.** `rookery server setup-key` on the server's machine prints the first admin's one-time link. Admins create links for others; a link works once, for 24 hours. OIDC is on when `ROOKERY_OIDC_ISSUER` and `ROOKERY_OIDC_CLIENT_ID` are set; the first sign-in of an email creates its user, and `ROOKERY_ADMINS` lists emails that start as admins. Browsers get a device session in an `HttpOnly`, `Secure`, `SameSite=Lax` cookie with a CSRF header. `rookery login URL` gets a device session through `/auth/cli` with a loopback redirect and PKCE. Access tokens are `rkpat_` plus 32 random bytes in base32, stored as SHA-256.
- **Groups** have members, added by hand or bound to an OIDC claim value (`ROOKERY_OIDC_GROUPS_CLAIM`), refreshed at each sign-in.
- **Resources** are URIs: `host://NAME`, `space://ID`, `record://PLUGIN/COLLECTION/ID`, `document://ID`, `plugin://ID`, `provisioner://NAME`, `rollout://ID`. A resource can have a parent, and a grant on a parent applies to its children.
- **Levels** are `view` ⊂ `use` ⊂ `manage`, on a resource, for a user, a group or `everyone`. The creator of a host, space or document gets `manage`. Admins have every level everywhere. On a host: `view` sees it, its state and desktop screenshots; `use` opens terminals, the desktop, exec as the desktop user, files and ports; `manage` runs as root or the operator, applies, runs actions, shares, renames and removes.
- **Principals** are `user:ID`, `plugin:ID`, and the Unix socket's local admin. When a plugin service handles a proxied call or a job for a user, it receives a caller token (signed by the server, bound to the plugin and the user, five minutes); passed back in the `Rookery-Caller` header, the plugin acts as that user, limited to what both may do.
- **Spaces** are containers with a kind that plugins declare, members through grants, and records and documents scoped to them. The client's current space is the `Rookery-Space` header.
- **Audit**: the interceptor records every call that changes state, with principal, procedure, resource and outcome.

## Data

Postgres 17 with pgvector. Schema `rookery` for the core, `p_<plugin>` for each plugin's collections, `p_<plugin>_sql` for raw SQL. All core tables are in `internal/store/schema.sql`.

### Core tables

`meta`, `users`, `links`, `sessions`, `tokens`, `groups`, `group_members`, `group_claims`, `grants`, `spaces`, `audit`; `hosts`, `host_labels`, `known_hosts`, `keys`, `host_cursors`, `provisioners`, `resources`, `tailnet_state`; `jobs`, `job_lines`, `schedules`, `rollouts`, `rollout_hosts`; `topics`, `topic_messages`, `topic_cursors`, `leases`, `notifications`; `settings`, `secrets`, `connections`, `blobs`; `plugins`, `plugin_kv`; `changes`, `refs`, `entities`, `document_updates`, `document_snapshots`.

### Datastore

Plugins declare collections in their manifest. The core owns the tables, so every write goes through one path that enforces access, keeps references and entities right, and feeds the change log.

- **Declaration.** A collection has fields with types (`string`, `text` (searchable), `int`, `float`, `bool`, `time`, `enum`, `ref` (to a record or any resource URI), `vector(N)`, `cbor`), required and default, indexes, a scope (`global`, `space`, `user`), access per operation as a level on the scope's resource, an `entity` block (kind, title field, text fields) when its records are entities, and `history: true` when changes are kept.
- **Tables.** Each collection is `p_<plugin>.<collection>` with `id` (ULID), `space_id` or `owner_id` by scope, `rev`, `created_at`, `updated_at`, `deleted_at`, and a column per field, created from the declaration at install.
- **Reads.** `Find` takes a filter, sort, limit and cursor, and returns records as CBOR maps. The filter language in `internal/filter` has comparisons, `in`, `contains`, `prefix`, `and`, `or`, `not`, on declared fields, compiled to SQL with parameters and to an in-memory matcher. A differential test generates filters and records and checks that both agree. `Aggregate` does `count`, `sum`, `min`, `max`, `avg`, grouped.
- **Writes.** Each write checks the revision when given one, validates against the declaration, and writes the row, its history row, its `refs` rows, its `entities` row and a `changes` row in one transaction. `Batch` runs several in one transaction.
- **Change feed and live queries.** `changes` has a global sequence. After commit the writer sends `NOTIFY rookery_changes`; one feed loop on the leader reads new changes in order and fans them out in memory. `Changes` streams from a cursor. `Live` runs the query, then matches each change in the collection against the filter and the subscriber's access, and sends inserts, updates and removals. A subscriber that falls behind gets `resync`.
- **References.** `RefsTo` and `RefsFrom` read `refs`, filtered by access. Deleting a referenced record follows the field's `onDelete` (`restrict`, `nullify`, `cascade`).
- **Entities and search.** `entities` holds each entity's URI, kind, title, a `tsvector`, an optional embedding and its access parent. `EntityService.Search` is full text (`websearch_to_tsquery`) plus vector similarity when a query embedding is given, filtered by access. Plugins write embeddings; the core computes none.
- **Raw SQL.** A plugin that declares `database: sql` gets a Postgres role that owns `p_<plugin>_sql` and can read its own collections. `ServerService.DatabaseCredentials` gives its service the connection string. The server's database user needs `CREATEROLE`.

### Live documents

The core stores and relays CRDT updates without reading them, so Yjs or another CRDT works.

- A document has an id, a type, a parent resource for access, and an append-only log of updates.
- `Sync` (bidi gRPC, or `/ws/documents/{id}`) sends the latest snapshot and every later update, then relays each update a participant sends to the others after appending it. Awareness messages are relayed, not stored.
- `Snapshot` stores a compacted state with the sequence it covers, written by a client or a plugin service; updates it covers are deleted after a day. `History` lists snapshots marked as versions.

### Blobs

Content-addressed by SHA-256, in `ROOKERY_BLOBS` (a directory, or `s3://bucket/prefix`), with a row for size, type, owner and access parent. A blob with no referencing row and no pin is removed after a day.

### Tests and Postgres

`internal/store/storetest` makes one template database per test binary from `schema.sql`, and a fresh clone of it per test (`CREATE DATABASE … TEMPLATE …`), dropped at cleanup. It reads `ROOKERY_TEST_POSTGRES`; unset, the test is skipped locally and fails when `CI` is set. `make db` runs `pgvector/pgvector:pg17` with `fsync=off` on `127.0.0.1:55432`.

## Jobs

- A job has a kind, CBOR arguments, a resource, who asked, a parent (a roll-out or another job), state (`queued`, `running`, `succeeded`, `failed`, `cancelled`, `interrupted`), attempts, a CBOR result, and log lines with a sequence.
- **Core kinds** (`host.contact`, `host.apply`, `host.action`, `host.create`, `host.remove`, `provisioner.action`) run in the server, one attempt, no automatic retry, at most one per host at a time.
- **Plugin kinds** are declared with who may enqueue them, a timeout and retries with backoff. The plugin's service works them with `ServerService.Work` (a server stream of claimed jobs) and reports lines, progress and the result. Claims use `FOR UPDATE SKIP LOCKED` with a heartbeat; a missed heartbeat returns the job to the queue.
- **Follow** streams lines from an offset, then the end state. Secrets a job was given are masked in its lines.
- **Schedules** run a job kind or a roll-out on a cron expression in a time zone. Missed runs are enqueued once.
- **Webhooks** are declared by plugins with a name, a verification (`hmac-sha256` over the body with a header and a secret name, or a `token` header) and a job kind. `/hooks/{plugin}/{name}` verifies, stores the body as the job's argument and enqueues it.
- Records and lines older than `ROOKERY_RETENTION_DAYS` (90) are deleted.

## Roll-outs

A roll-out applies, or runs one action on, a set of hosts in batches.

- **Selection**: host names, a label selector (`role=desk,site!=lab`), or `behind` (hosts whose applied digest is not current), combined. `Plan` resolves it and returns the hosts, skipping those the type or capability check rules out with the reason.
- **Strategy**: batch size (a count or a percentage), the number of failures that pauses it (default 1), an optional pause between batches, and an optional canary (the first N hosts as a batch of their own, then a pause for `Resume`).
- **Running**: the roll-out is a record with a state (`running`, `paused`, `succeeded`, `failed`, `cancelled`) and a row per host. Each host's work is a child job (`host.apply` or `host.action`) with the roll-out as parent. `Watch` streams the roll-out's and its hosts' progress. `Pause` lets running jobs finish and starts no more; `Cancel` cancels the running jobs too.
- After a leader change a running roll-out is paused, never resumed by itself.
- Plugins start roll-outs through the same service, and schedules can start them.

## Messaging

- **Events** are at most once, in memory on the leader: `Publish` from plugins, `Subscribe` from clients and plugins, by name with a prefix match. Core events: `rookery/hosts.changed`, `rookery/jobs.changed`, `rookery/rollouts.changed`, `rookery/plugins.changed`. Subscribers see only events about resources they can view.
- **Topics** are durable: messages in `topic_messages`, a cursor per consumer group, `Ack`, `Nack` with redelivery after a delay, dead letters after a set number of tries, and `Inspect`, `Retry`, `Discard` for admins. Host channel messages arrive in topics named `host/<name>/<topic>`.
- **Leases** are named locks with a holder, a TTL and a fencing number that rises on each acquire.
- **Notifications** are per user: title, body, link, resource, optional actions to answer, read state. Plugins send them; the core sends them for failed jobs and paused roll-outs the user started. `Watch` streams new ones.

## Settings, secrets and connections

- **Settings** are declared by the core and plugins with a type, a default and scopes (`global`, `space`, `user`). The value for a caller resolves user, then space, then global, then default. Values are CBOR.
- **Secrets** are sealed with XChaCha20-Poly1305 under the deployment key, with the row's id as associated data and a key id per row, scoped `global`, `plugin/<id>`, `provisioner/<name>` or `host/<name>`. Admins set them and list their names; values go only to the apply, provisioners, the owning plugin, and for `host/<name>` to that host's `box secret`. `rookery server secrets rekey` reseals every row under a new key.
- **Connections** are external credentials of a type the core or a plugin declares (fields, which are secret, an optional test on the plugin's service). The core declares `registry` (a container or package registry login, which the apply writes on hosts that name it).

## Hosts

### Types

| Type | Has | Rookery can |
| --- | --- | --- |
| `ssh` | SSH only | Exec over SSH, first contact, apply (which makes it at least `managed`) |
| `managed` | The base role: `rookeryd`, `box`, the tailnet | Apply with features, actions, terminals, exec, files, ports, channel, host services, engines for provisioners |
| `workstation` | `managed` plus the desktop role | Everything, with the desktop |

The table is data in `internal/hosts/types.go` with one function, `Check(host, capability) error`, which every service, roll-out plans and the CLI ask. A refusal says what would change the answer.

### Profile and labels

`HostProfile`: `type`, `address`, `port`, `ssh_user`, `operator` (default `ops`), `desktop` (for workstations: the desktop user, default `dev`, screen size, GPU on or off), `tailnet` (default true), `registries` (connection names), `features` (plugin, feature, `vars_cbor`), `vars_cbor` for the base role. Labels are key-value pairs on the host record, outside the profile, used by roll-outs and filters. The applied digest is the SHA-256 of the deterministic CBOR of the profile, the versions of the plugins it names, and Rookery's version; a host whose stored digest differs is behind.

### Life of a host

1. **Added** (`rookery host add forge 192.0.2.4 --user ops`) as `ssh`, or **created** through a provisioner (`rookery host create desk-3 --provisioner lab-incus --type workstation`), whose seed joins the tailnet.
2. **First contact**, a job: connect with the key ring's key; if refused, take a password or private key once from the CLI's stdin and install the ring's public key. Record the host key, trusted on first use and logged with its fingerprint. A later mismatch fails every link until an admin trusts the new key.
3. **Applied**: the base role, the desktop role for a workstation, then the profile's features. The first apply installs `rookeryd` and `box` and joins the tailnet.
4. **Used**: desktop, terminals, exec, files, ports, channel and host services through the server. A client never needs a route to a host.
5. **Removed**: the machine through its provisioner when it has one, the tailnet device, then the record with its grants and keys.

### Tailnet

The server is a node of one tailnet through `tsnet`, with its node state in `tailnet_state` (a custom `ipn.StateStore`), so a standby takes the same identity. The tailnet is Tailscale's hosted service or a self-hosted Headscale, set with `TailnetService.Configure`: control URL, API key (a secret), and the tag hosts get.

- For each host that joins, the server mints a one-use, pre-authorised, tagged auth key, passes it to the seed or the apply, and records the device id and address when the host appears.
- Links dial the tailnet address through `tsnet` when the host has one, the profile's address otherwise. Ansible's SSH reaches tailnet hosts with `ProxyCommand=rookery server dial %h %p`, which pipes through `LocalService.Dial` on the server's socket and out through `tsnet`.
- Removing a host deletes its device. `ListDevices` shows tagged devices with no host, for cleanup.
- `internal/tailnet` exposes `Dialer` and `Control` interfaces. Tests use fakes; nothing in CI talks to a real control server.

OpenZiti was considered: it embeds in Go on both ends too, but needs its own controller, routers and PKI, and its advantage, policy per service through its API, duplicates what Rookery's edge enforces, since every client goes through the server.

### Links

`internal/link` holds at most one SSH connection per host, opened on demand and closed after five idle minutes, except for hosts with a channel follower, which stay open. A session on it runs `sudo -n rookeryd connect`; `pipeconn` puts a gRPC client on it, and calls share it as HTTP/2 streams. SSH uses the key ring through `x/crypto/ssh`, with host keys checked against `known_hosts`. Only the leader holds links.

## On the host

### `rookeryd`

`rookeryd serve` runs as root under systemd. It has no database; its only state is the channel store (`/var/lib/rookeryd/channel.db`, bbolt). It listens on two sockets:

- `/run/rookeryd/root.sock`, mode 0600: the server, through `rookeryd connect`. Every service below, for any user.
- `/run/rookeryd/user.sock`, mode 0666: `box` and other local programs. It reads the caller's uid with `SO_PEERCRED` and allows what that user may do: their own sessions, the channel, host services, desktop control when they are the desktop user, the actions marked for users, and their own host secrets.

| Service | RPCs | Notes |
| --- | --- | --- |
| `AgentService` | `Version` | Version, OS, architecture, uptime, desktop state |
| `SessionService` | `Create`, `List`, `Attach` (bidi), `Resize`, `Signal`, `Close` | A PTY per session, as a given user, outliving links. The last 1 MiB of output in a ring buffer; `Attach` replays from an offset, then streams. No terminal emulation |
| `ExecService` | `Run` (server stream) | A command as a given user, with stdin, environment, directory, timeout, and optionally the desktop's environment |
| `FileService` | `Read`, `Write`, `List`, `Stat`, `Delete` | As a given user, with a revision for compare-and-swap writes |
| `PortService` | `Dial` (bidi bytes) | To `127.0.0.1:PORT` or a Unix socket path |
| `ChannelService` | `Publish`, `Read`, `Follow` (server stream from a sequence) | Messages on topics `PLUGIN/TOPIC` or `user/TOPIC`, each kept as `none`, `last` or `log` (bounded to 10,000 per topic) |
| `HostCallService` | `Register` (bidi: the provider receives calls and answers), `List`, `Call` | Host services: a local program registers a named service with methods; the server and other local programs call it with CBOR. Calls to a service that isn't registered fail with `Unavailable` |
| `DesktopService` | `Screenshot`, `Input`, `GetClipboard`, `SetClipboard`, `Pause`, `Resume`, `State` | Relayed to the desktop provider, a host service that `box desktop serve` registers from inside the desktop session |
| `ActionService` | `List`, `Run` (server stream) | The actions of the host's features marked `onHost: true`, as the calling user |

The desktop's RFB stream doesn't go through `DesktopService`: the server dials the desktop's VNC Unix socket with `PortService.Dial`.

The server follows each managed host's channel (`ChannelService.Follow` from the cursor in `host_cursors`) and publishes the messages to topics `host/<name>/<topic>`. `ChannelService.Publish` on the edge writes to a host's channel.

The server's terminal records (id, host, owner, level to attach, viewers) are in memory and rebuilt from `SessionService.List` when a link opens.

### `box`

`box` is the tool for users and programs on a host. It talks only to `rookeryd`'s user socket.

```
box desktop screenshot [--output PATH|-] | click X Y [BUTTON] | type TEXT | key CHORD | scroll up|down [N]
box desktop clipboard [--set] | pause | resume | state
box desktop serve                       the desktop provider, run by the desktop session's systemd user unit
box exec [COMMAND…]                     with the desktop's display, Xauthority, session bus and PATH
box actions | run ACTION [NAME=VALUE…]
box publish TOPIC [VALUE] [--keep none|last|log] | listen PREFIX [--since SEQ]
box services | call SERVICE METHOD [VALUE] | serve SERVICE -- COMMAND…
box secret NAME                         a host/<name> secret granted to this user, on stdout
box version
box <name> …                            runs box-<name> from /usr/lib/rookery/box/, for plugins
```

Values on the command line and stdout are JSON for people and scripts; `box` converts them to and from CBOR. `box serve SERVICE -- COMMAND` registers a host service and runs `COMMAND METHOD` per call with the request on stdin and the result on stdout.

`box desktop serve` runs in the desktop session as the desktop user. It registers the desktop provider, takes screenshots with `GetImage` and input with XTest through `internal/x11`, reads and writes the clipboard through the X selection owned by a small window it keeps, and serialises input per call; `pause` makes cooperating callers wait until `resume`.

### Extension points on hosts

Plugins extend hosts without changes to `rookeryd` or `box`:

- **Host features** install programs with Ansible.
- **Actions** of features, for operators through the server and, when marked `onHost: true`, for users on the host through `box run`.
- **Host services**: a feature installs a systemd unit that runs `box serve NAME -- COMMAND` or a program that calls `HostCallService.Register` itself; the server and plugins reach it with `HostCallService.Call`.
- **Channel topics** under the plugin's id, followed into the server's topics.
- **`box` subcommands**: a feature installs `/usr/lib/rookery/box/box-<name>`.

## Desktops

A workstation has one shared virtual desktop for its desktop user. The `desktop` role, which the apply runs for workstations:

- Installs XFCE, TigerVNC (`Xvnc`), Chromium, `dbus-x11`, Mesa, and optionally gives the desktop user the GPU (`render` and `video` groups).
- Runs the desktop as systemd user units of the desktop user, with lingering on: `rookery-desktop.service` starts `Xvnc :1` with the profile's screen size, `-rfbunixpath /run/user/<uid>/rookery/vnc.sock` (mode 0600), `-SecurityTypes None` and no TCP listener, then the XFCE session; `rookery-box-desktop.service` runs `box desktop serve`.
- Turns off screen locking and power management in that session, gives the user an unlocked default keyring, and writes `/etc/rookery/desktop.env` with the display, Xauthority and bus address that `box exec` and `ExecService` use.

On the server:

- `/ws/hosts/{name}/desktop` (needs `use`) dials the VNC socket through `PortService.Dial` and passes RFB bytes over the WebSocket. A client half shows it with noVNC. `DesktopService.Connect` does the same for `rookery desktop NAME`, which listens on a local port for a VNC viewer.
- `DesktopService.Screenshot` (needs `view`) returns a PNG, scaled when asked, for thumbnails.
- `DesktopService.Input`, `GetClipboard`, `SetClipboard`, `Pause` and `Resume` (need `use`) relay to `rookeryd`, which relays to the provider.

Tests run `box desktop serve` against `Xvfb` in the `x11` and `agent` packages when `Xvfb` is on `PATH` (it is in CI), skipping locally otherwise.

## Apply

A job of kind `host.apply`:

1. A work directory under `ROOKERY_STATE_DIR/jobs/<id>`, mode 0700, removed afterwards.
2. The active private key and the host's known key as files of mode 0600.
3. `inventory.yml` with the host, its tailnet or profile address, port, user, and SSH options using those files, `StrictHostKeyChecking=yes`, and the tailnet `ProxyCommand` when it applies.
4. The embedded `ansible/` roles extracted, and each feature's tasks linked in from the installed plugin folder as a role `<plugin>__<feature>`.
5. A generated `site.yml`: `base`, `desktop` for a workstation, then the features in profile order.
6. `vars.json` (mode 0600): the profile's and features' variables decoded from CBOR, the secrets and registry connections they declare, a fresh tailnet auth key, and the paths of the right `rookeryd` and `box` binaries. Secrets go on the job's mask list.
7. `ansible-playbook -i inventory.yml site.yml -e @vars.json` with a generated `ansible.cfg` (pipelining, no retry files, no colour). Each output line is masked and logged.
8. On success: store the applied digest, read `AgentService.Version`, set the type from what the host now has.

`ansible-core` is pinned in the server's image and runs only on the server. The `base` role, written fresh for Ubuntu 24.04 and Debian 12: the operator account with the ring's key and passwordless sudo, SSH settings, Tailscale joined with the auth key, `rookeryd` with its unit and `box`, registry logins, unattended upgrades, and a firewall that allows SSH and the tailnet. The old role in `rookery/provision/ansible/roles/rookery/` can be read for what it did, the desktop pieces in `tasks/desktop.yml`, `templates/desktop.service.j2` and `files/` especially; toolchains, coding clients and agent accounts in it are future plugins' features.

`rookeryd` and `box` are built with `CGO_ENABLED=0` for `linux/amd64` and `linux/arm64`. The server finds them in `ROOKERY_RELEASE_DIR`, by default beside its own executable.

## Actions

A host feature declares actions: name, label, group, user (`operator`, `root` or `desktop`), `onHost: true` when users on the host may run it through `box`, whether it needs a terminal, its arguments, and the command. `ListActions` returns the core actions (`status`, `update`, `reboot`, and for workstations `restart-desktop`) and those of the host's features. `RunAction` runs one as a `host.action` job through `ExecService`, or for a terminal action opens a terminal running it. Arguments arrive as CBOR and reach the command as `ROOKERY_ARG_<NAME>` variables, never interpolated into a shell string.

## Provisioners

A plugin declares provisioner types, each with the resource kinds it makes. `machine` is the kind the core knows: a machine for a host, at most one per host. Other kinds (such as Docker's `deployment`) are resources the core records, lists and removes without knowing more.

An admin adds a provisioner entry with a name, a type and arguments; its secrets are scoped `provisioner/<name>`. An entry is either on a **node**, a managed host that runs the engine (the provisioner then reaches the engine's Unix socket through `PortService.Connect`, which the server gives it as a dialer), or **remote**, with an address and credentials in its arguments. A host feature that installs an engine declares the provisioner types it offers; applying it to a node adds entries named `<node>-<type>` for it.

A provisioner is an executable in the plugin package. The server starts it for a call with `ROOKERY_PLUGIN_SOCKET`, `ROOKERY_SOCKET` and a token, calls `ProvisionerService`, and stops it afterwards. Its stderr goes to the job's log.

| RPC | Does |
| --- | --- |
| `Describe` | Kinds, arguments and defaults, size and image choices, the secrets it needs, its actions |
| `Status` | Whether the engine answers, with a message |
| `Create` | One resource of a kind from arguments, and for a machine the seed and the host's name; returns state as CBOR and, for a machine, its address once known |
| `Get`, `Start`, `Stop`, `Remove` | From the stored state |
| `List` | What it made, matched to resources by the labels it sets |
| `RunAction` | A declared action with CBOR arguments, streaming log lines |

The seed is cloud-init user data that Rookery renders: the operator account with the ring's public key and passwordless sudo, and Tailscale joined with a one-use auth key. Everything else is the apply.

### Incus

`plugins/incus`, its own module, bundled and installed by default:

- Host feature `server` (for managed hosts): installs Incus from the Zabbly repository, initialises a bridge `incusbr0` and a storage pool, and offers `incus/vm` and `incus/container`.
- Provisioner types `incus/vm` (KVM guests) and `incus/container` (system containers), both of kind `machine`, with `cloud-init.user-data` set to the seed, sizes (CPU, memory, disk), images (Ubuntu 24.04, Debian 12 cloud images), the profile and project, and labels `user.rookery.*`. The client is `github.com/lxc/incus/v6/client` over the node's `/var/lib/incus/unix.socket`, or an HTTPS address with a client certificate when remote.
- Actions: images, storage pools, networks, and removing instances Rookery has no record of.

### Docker

`plugins/docker`, its own module, bundled and installed by default:

- Host feature `engine` (for managed hosts): installs Docker Engine from Docker's repository, optionally the sysbox runtime, and offers `docker/container`.
- Provisioner type `docker/container` with two kinds:
  - `machine`: a container from Rookery's machine image (`images/machine.Dockerfile`: Ubuntu 24.04 with systemd, SSH, Python and cloud-init's `NoCloud` source reading a mounted seed), run with the `sysbox-runc` runtime when the engine has it, or privileged when the entry's `privileged` argument allows it; a named volume for its disk; its address on a Docker network the plugin creates.
  - `deployment`: a Compose project from a file given as an argument, run with `compose-go` and the Docker API, with start, stop, logs and remove.
- The client is `github.com/docker/docker/client` with a dialer over the node's `/var/run/docker.sock`, or `local` for the server's own engine.
- Actions: images, pull, volumes, and pruning what Rookery left behind.

## Plugins

### Package

A `.tar.gz` with `plugin.yaml` at its root and the files it names. `plugins.Load(fs.FS)` parses and validates it; `rookery plugin check`, the installer and the server all use it. A JSON Schema of the manifest is generated from the Go types.

```yaml
id: example
version: 0.1.0
name: Example
requires: ">=0.1.0"
dependencies: { other: "^1.0" }

service:
  command: { linux/amd64: bin/service-amd64, linux/arm64: bin/service-arm64 }
  # or: runtime: bun, entry: service/main.js
  start: lazy               # or: always
  rpc:
    - procedure: example.v1.NoteService/Summarize
      access: { level: view, resource: space }

collections:
  notes:
    scope: space
    access: { read: view, write: use }
    history: true
    fields:
      title: { type: text, required: true }
      body: { type: text }
      done: { type: bool, default: false }
      about: { type: ref }
    indexes: [[done]]
    entity: { kind: Note, title: title, text: [title, body] }
database: sql

spaceKinds: [{ id: notebook, label: Notebook }]
settings: { greeting: { type: string, default: hello, scopes: [global, user] } }
secrets: [example_token]
connectionTypes: [{ id: notes-api, fields: { url: {}, token: { secret: true } }, test: true }]
jobs: [{ kind: reindex, enqueue: { level: manage, resource: space }, timeout: 10m, retries: 3 }]
schedules: [{ kind: reindex, cron: "0 3 * * *" }]
webhooks: [{ name: push, verify: { hmac-sha256: { header: X-Signature, secret: example_token } }, job: reindex }]
events: [example/note.changed]

hostFeatures:
  - id: hello
    label: Hello
    types: [managed, workstation]
    tasks: features/hello/tasks.yml
    vars: { greeting: { type: string, default: hello } }
    secrets: [example_token]
    offers: []                          # provisioner types this feature's engine serves
    hostServices: [example/hello]       # what it registers, for listings
    channelTopics: [example/progress]
    actions:
      - { name: say, label: Say hello, user: operator, run: cat /etc/example-hello }
      - { name: wave, label: Wave, user: desktop, onHost: true, run: example-wave }
provisioners:
  - id: fake
    label: Fake machine
    kinds: [machine]
    command: { linux/amd64: bin/provisioner-amd64, linux/arm64: bin/provisioner-arm64 }

client: web/
shell: false
contributes:
  views: [{ id: notes, label: Notes, route: /notes, space: notebook }]
  nav: [{ view: notes, group: Workspace }]
  hostTabs: [{ id: hello, label: Hello, types: [workstation] }]
  widgets: []
  adminPages: []
```

### Install and registries

- `rookery plugin add ./example.tar.gz` streams the package. `rookery plugin add example@0.1.0` resolves it from the configured registries: static HTTPS directories with `index.json` (ids, versions, URLs, SHA-256, minimum Rookery version) and `index.json.sig`, an Ed25519 signature checked against the registry's key. `rookery plugin pack DIR` writes a package and its index line; `rookery registry index DIR --key FILE` writes and signs an index.
- Installing checks the manifest, the hash, versions and dependencies, and that the id is free; stores the package in `plugins`; unpacks it into `ROOKERY_STATE_DIR/plugins/<id>/<sha256>/`; creates its tables, roles and schemas in one transaction; registers job kinds, schedules, webhooks, settings, connection types and space kinds; starts the service.
- An update with the same collection declarations swaps the package and restarts the service. Different declarations are refused without `--reset-data` until the first tagged release.
- Removing refuses while a host profile, provisioner entry or dependent plugin uses it; its data is kept until `--purge`.
- The release bundles `docker` and `incus` as packages beside the server; the server installs them on an empty database.

### Services

A service is the plugin's server-side code, in any language with gRPC. The server starts it (at install or on first use), restarts it with backoff when it exits, and stops it on remove, disable or shutdown. It runs as the server's user with an empty environment except `PATH`, `HOME`, `LANG`, `TZ`, `ROOKERY_SOCKET`, `ROOKERY_PLUGIN_SOCKET`, `ROOKERY_PLUGIN_TOKEN`. Its output goes to the server's log with the plugin's id. A service over `ROOKERY_SERVICE_MEMORY_MB` (512) resident is restarted.

On `ROOKERY_PLUGIN_SOCKET` it serves `PluginService` (`Init` with id, version and settings; `Health`; `TestConnection`) and its declared procedures. On `ROOKERY_SOCKET`, with its token, it calls every core service as `plugin:<id>` (or as a user with a caller token), and `ServerService` for what only plugins get: its own secrets, settings and `plugin_kv` with revisions and compare-and-set; `Publish` events; topics and leases; `Work` for its job kinds; `SendNotification`; `RegisterEntity`; `DatabaseCredentials`.

### Client halves and the shell

The frontend contract is `@rookery/client`: a page loads it, it signs in through the cookie, talks to the edge with Connect, and loads each enabled plugin's client half from `/plugins/<id>/<hash>/index.js`. A client half exports `defineClient({ views, widgets, hostTabs, … })` from `@rookery/sdk/client`, receiving a context with the typed API clients, the current user and space, live queries, documents, terminals, the desktop stream, and navigation. `@rookery/client/solid` gives SolidJS 2 bindings (signals and async primitives over live queries, documents and streams), written against Solid 2's APIs with no 1.x compatibility layer. The shell is the one installed plugin with `shell: true`; Rookery serves its `index.html` for `/` and unmatched paths, and `PluginService.Contributions` gives it every enabled plugin's `contributes` and hashes. Rookery checks this contract with a fixture shell, not with a UI of its own.

### Fixture plugins

`testdata/plugins/` holds the plugins the core's own tests use:

- `example` (Go, on `pkg/sdk`): everything in the manifest above. Its service implements `NoteService/Summarize`, works `reindex`, takes the webhook, uses kv, events, raw SQL and a notification. Its host feature registers a host service with `box serve` and publishes on its channel topic. Its `fake` provisioner keeps machines in its own state and returns the address of a test host.
- `example-ts` (TypeScript on `@rookery/sdk`, run with Bun): a collection, a procedure, a job kind and a client half.
- `shell`: a plain HTML page that loads `@rookery/client`, lists contributions and mounts a client half's view.

## SDKs

The protos are the contract; every SDK's API types are generated with buf. Each SDK adds the same small runtime by hand: listen on `ROOKERY_PLUGIN_SOCKET` and serve `PluginService` plus the plugin's procedures; connect to `ROOKERY_SOCKET` with the token; CBOR helpers; typed collection access from the manifest; a job worker loop; caller-token passing; and a test harness that runs the plugin against a real `rookery serve` started by the test.

| SDK | Package | Covers |
| --- | --- | --- |
| Go | `pkg/sdk` | Services, provisioners (with the node dialer), workers, host services through `rookeryd`'s user socket, the test harness |
| TypeScript | `@rookery/proto`, `@rookery/sdk` (services on Bun or Node, and `defineClient`), `@rookery/client` (transport, sign-in, live queries, documents, terminals and desktops over WebSocket, the client-half loader), `@rookery/client/solid` (SolidJS 2) | Services and the browser |
| Python | `rookery-sdk` on `grpcio`, `cbor2` | Services, workers |
| JVM | `dev.rookery:sdk` on `grpc-java`, `jackson-dataformat-cbor`, usable from Kotlin | Services, workers |
| .NET | `Rookery.Sdk` on `Grpc.Net`, `System.Formats.Cbor` | Services, workers |
| Rust | `rookery-sdk` on `tonic`, `ciborium` | Services, workers |

`sdk/conformance/` has a spec in proto (`conformance.v1.ConformanceService`) and a Go runner. Each SDK has a conformance plugin; the runner installs it into a test server and checks registration, procedures with and without caller tokens, collections, live queries, a job with retries, kv compare-and-set, events and errors. A new SDK is done when it passes.

## CLI

`rookery` reads `ROOKERY_SERVER` and `ROOKERY_TOKEN`, or the session saved by `rookery login` under `os.UserConfigDir()`. On the server's machine it uses the Unix socket when it can.

```
rookery login URL [--token-stdin] | logout | whoami
rookery serve
rookery server setup-key | token --user EMAIL --name NAME | secrets rekey | reset | dial HOST PORT
rookery user list | link EMAIL | admin EMAIL [--off] | disable EMAIL
rookery group list | create NAME | add NAME EMAIL | remove NAME EMAIL | bind NAME CLAIM
rookery space list | create KIND NAME | rename ID NAME | archive ID
rookery grant RESOURCE (--user EMAIL | --group NAME | --everyone) --level L | revoke … | grants RESOURCE
rookery host list [--label K=V] | show NAME | add NAME ADDRESS [--user U] [--port P] [--type T]
rookery host create NAME --provisioner P [--type T] [ARG=VALUE…] | remove NAME | rename NAME NEW
rookery host profile NAME [--edit] | label NAME K=V… [--remove K…]
rookery apply NAME | run NAME ACTION [ARG=VALUE…] | exec NAME [--as root|operator|desktop] -- CMD… | ssh NAME
rookery desktop NAME [--local ADDR] | screenshot NAME [--output PATH]
rookery port NAME PORT [--local ADDR] | cp SRC DST
rookery channel publish NAME TOPIC VALUE | listen NAME PREFIX
rookery call NAME SERVICE METHOD [VALUE]
rookery rollout plan|start (--hosts A,B | --label K=V | --behind) [--action A] [--batch N|P%] [--max-failures N] [--canary N] [--wait D]
rookery rollout list | show ID [--follow] | pause ID | resume ID | cancel ID
rookery job list | show ID | logs ID [--follow] | cancel ID | retry ID
rookery schedule list | run ID
rookery key list | rotate | trust NAME
rookery tailnet status | configure --control URL --api-key-stdin --tag TAG | devices
rookery provisioner types | list | show NAME | add NAME TYPE [--node HOST] [ARG=VALUE…] | set NAME ARG=VALUE…
rookery provisioner remove NAME | resources NAME | run NAME ACTION [ARG=VALUE…]
rookery secret list | set NAME | remove NAME
rookery setting list | get NAME | set NAME VALUE
rookery connection types | list | add TYPE NAME | test NAME | remove NAME
rookery plugin list | add PACKAGE|ID[@VERSION] | update ID [--reset-data] | remove ID [--purge] | enable ID | disable ID | check DIR | pack DIR
rookery registry add NAME URL --key KEY | list | remove NAME | index DIR --key FILE
rookery data find PLUGIN/COLLECTION [--where FILTER] | get … | live …
```

Plain text for people, `--json` (protojson) for scripts. Commands that start a job or a roll-out follow it and exit with its result.

## Tests and CI

The aim: the core's Go suite in under 30 seconds warm on a laptop, CI under two minutes, no virtual machines.

| Layer | How |
| --- | --- |
| Pure logic: manifest, filter, digest, capability table, CBOR, masking, inventory and playbook, cron, roll-out batching | Unit tests |
| Filter | The differential test of SQL against the in-memory matcher |
| Store and datastore | `storetest` databases |
| Services | Real connect clients against handlers on `httptest`, with the access interceptor, on `storetest` |
| Live queries, feed, documents | Writers and subscribers in one test: order, access filtering, resync |
| Roll-outs | A fake job runner: batches, canary, failure threshold, pause, resume, cancel, leader change |
| `pipeconn` | `net.Pipe`: half-close, large messages, cancellation |
| Links | An in-process SSH server from `x/crypto/ssh` running `rookeryd`'s handlers for `rookeryd connect`: first contact, key mismatch, idle close, reconnect |
| Tailnet | Fake `Control` and `Dialer` |
| `rookeryd` and `box` | Both sockets in a short temp directory, real PTYs and commands as the test's user, `SO_PEERCRED` checks, channel keep policies, host service registration and calls, `box` commands run in-process |
| Desktop | `box desktop serve` on `Xvfb`: screenshot, input, clipboard, pause; the RFB proxy against a fake RFB socket |
| Apply | A fake `ansible-playbook` test binary first on `PATH`, recording arguments, files and environment |
| Ansible | `ansible-playbook --syntax-check` and `ansible-lint` on `ansible/` and the plugins' features |
| Plugins | The fixture plugins: install, update, remove refusals, service restart, procedures with access, jobs, schedules, webhooks, kv, events, notifications, the fake provisioner |
| Leader | Two servers on one database: the second answers 503 and takes over when the first exits |
| Docker plugin | Its provisioner against the CI machine's Docker engine: create a `machine` and a `deployment`, list, stop, remove |
| Incus plugin | Its provisioner against a fake of the Incus REST API on `httptest`, from responses recorded from a real Incus and kept in `testdata` |
| TypeScript | `bun test` and `tsc --noEmit` in `sdk/ts`; one Playwright spec loading the fixture shell against a test server |
| SDKs | Conformance per SDK |
| End to end | One Go test, build tag `e2e` |

### End to end

`dev/compose.yaml` runs Postgres and the test host image (`images/testhost.Dockerfile`: Ubuntu 24.04 with systemd as PID 1, SSH, Python, sudo, and the packages of the base and desktop roles already installed), privileged with its own cgroup namespace. The `e2e` test builds `rookery`, `rookeryd`, `box` and the bundled plugins, starts the server on the machine running the test with the tailnet off, and drives the CLI:

1. Setup link, token sign-in, `host add` with a password from a file, first contact.
2. `apply` as a workstation; the host is a workstation; `run status`, `exec`, a terminal round trip, a port forward.
3. A desktop screenshot that isn't blank, a click through `DesktopService.Input`, and RFB bytes through `/ws/hosts/{name}/desktop`.
4. `box publish` on the host arrives in the server's topic; `box serve` answers `rookery call`.
5. Apply the `docker` plugin's `engine` feature to the host; it becomes a node with `<host>-docker/container`; `host create` a managed machine through it; apply it.
6. A roll-out over both hosts with `--batch 1`.
7. Install `example`, call `Summarize` through the edge, remove everything.

Under two minutes with a warm image. It is the only test with real Ansible, and the only one besides the Docker plugin's that needs Docker.

### Commands

```bash
make gen      # buf generate, sqlc generate, manifest JSON Schema
make db       # Postgres with pgvector in Docker
make reset    # drop and recreate the dev database's schemas
make test     # go test -race ./... in every module of the workspace
make lint     # golangci-lint, buf lint, ansible-lint
make ts       # bun install, typecheck, bun test in sdk/ts
make e2e      # go test -tags e2e ./e2e/...
make dev      # the dev stack and rookery serve against it, rebuilt on change
```

### CI

One workflow on pull requests and pushes, jobs in parallel:

- `go`: Go module and build caches, a `pgvector/pgvector:pg17` service, `Xvfb` installed; `make gen` with `git diff --exit-code`, `make lint`, `make test` for the core module, cross-builds of `rookery` for `darwin/arm64` and `windows/amd64` and of `rookeryd` and `box` for `linux/arm64`. Under 90 seconds warm.
- `plugins`: `make test` for `plugins/docker` and `plugins/incus`, with the runner's Docker engine. Only when those modules, `pkg/sdk` or `proto/` changed. Under 90 seconds.
- `ts`: Bun's cache; `make ts` and the Playwright spec. Under 60 seconds.
- `e2e`: the test host image cached by its Dockerfile's hash; `make e2e`. Under two minutes.
- `sdk-<lang>`: conformance for one SDK, only when `proto/`, `sdk/<lang>/` or `sdk/conformance/` changed.

A check that can't keep to these times is changed until it does.

## Order of construction

Each item is one pull request that leaves the branch green. An item depends on the ones before it unless it says otherwise.

1. **Skeleton.** `go.work`, layout, buf, sqlc, golangci-lint, `Makefile`, the `go` CI job, `AGENTS.md` with this file's conventions and commands.
2. **Foundations.** `cborx`, `pipeconn`, `store` with `schema.sql`, the digest check, `reset` and `storetest`, `seal`.
3. **Server, people and access.** Start-up, leader lock and standby, edge and socket, `authn`, groups, `access` with resources, inheritance, the method option and interceptor, the every-method test, spaces, audit, and their services and CLI.
4. **Jobs and messaging.** Jobs with core and test kinds, follow, cancel, retries, claims, schedules; events, topics, leases, notifications; settings, secrets, connections, blobs; their services and CLI.
5. **`rookeryd` and `box`.** `proto/rookery/host/v1`, both sockets with peer credentials, sessions, exec, files, ports, channel, host services, user actions, `box` without `desktop`. Independent of 3 and 4.
6. **Hosts and links.** Hosts, profiles, labels, types, `link`, key ring, known hosts, first contact, `HostService` without apply, terminals with WebSocket, exec, files, ports with both proxies, channel take-in into topics, `HostCallService`, host events, the CLI.
7. **Apply and actions.** `ansible/base`, `apply`, the digest, actions, registry connections. The test host image, the `e2e` test's first two steps, and the `e2e` CI job.
8. **Desktops.** `internal/x11`, `box desktop`, the desktop provider and `DesktopService` relay in `rookeryd`, `ansible/desktop`, the server's `DesktopService`, the RFB WebSocket route, `rookery desktop` and `screenshot`. The `e2e` test's third and fourth steps.
9. **Roll-outs.** `internal/rollouts`, `RolloutService`, schedules that start roll-outs, notifications on pause, the CLI.
10. **Tailnet.** `tsnet` with state in Postgres, `Control` for Tailscale and Headscale, auth keys in apply, dialing and `ProxyCommand` over the tailnet, device cleanup, `TailnetService`, the CLI.
11. **Datastore.** `filter` with its differential test, collection declarations and tables, reads, writes, history, refs, the change feed, live queries, entities and search, raw SQL roles, `DataService`, `EntityService`. Declared through a test-only manifest until item 13. Independent of 5 to 10.
12. **Live documents.** Update logs, snapshots, `Sync` over gRPC and WebSocket, `DocumentService`. Independent of 5 to 10.
13. **Plugins.** Manifest and schema, packages, registries and signatures, install, update, remove; services with supervision, `PluginService`, `ServerService`, caller tokens, procedures through the edge, job workers, webhooks, plugin settings, secrets, kv, connection types, space kinds; host features, their actions, host services and topics in the apply; client halves, the shell and `Contributions`. `pkg/sdk` and the `example` fixture. The `e2e` test's last step.
14. **Provisioners.** Entries on nodes and remote, resources and kinds, machines, the seed, `host create` and removal, features that offer types, the fixture's `fake`.
15. **Docker.** `plugins/docker` with the `engine` feature, the machine image, `machine` and `deployment` kinds, actions, its CI job, bundling. The `e2e` test's fifth and sixth steps.
16. **Incus.** `plugins/incus` with the `server` feature, `incus/vm` and `incus/container`, actions, the recorded fake, bundling. Independent of 15.
17. **TypeScript SDK.** `@rookery/proto`, `@rookery/sdk`, `@rookery/client` with `solid` on SolidJS 2, noVNC wiring for the desktop stream, the `example-ts` and `shell` fixtures, the `ts` CI job.
18. **Conformance and the other SDKs.** The spec and runner, passing for Go and TypeScript first, then Python, JVM, .NET and Rust, one PR each with its CI job.
19. **Release.** GoReleaser for `rookery` (Linux, macOS, Windows), `rookeryd` and `box` (Linux amd64, arm64) and the bundled plugins; the server's image with `ansible-core`, both host binaries and the bundled plugins; the machine image; `deploy/compose.yaml` with Postgres; npm and language packages from tags. The `go` branch replaces `main`. The first tag turns `schema.sql` into migration 1.

## How the two agents work

There is one orchestrator and one implementer.

The implementer writes the code. It takes the next item, works on a branch from `go` in its own worktree, and opens one pull request for it. Before opening it, `make gen lint test` passes, plus `make ts` or `make e2e` when the item touches them. The description says what the PR adds, what it leaves for a later item, and anything in this file it found wrong or missing.

The orchestrator stays out of the heavy work. It:

- Answers the implementer's questions from this file. When the file doesn't answer one, it decides, writes the answer into this file, and tells the implementer. The file stays the single source of truth.
- Reviews each pull request once against this file and the Go conventions: API shape, access options, secrets in logs or argv, calls without a context, goroutines without an owner, dependencies off the list, a test that needs a container outside `e2e` and the Docker plugin, CI over its budget. It fixes small findings itself rather than sending them back.
- Merges on green after that review and starts the implementer on the next item at once. Items marked independent can be taken out of order when the implementer is blocked.
- Keeps `rookery/` in `mono` pointing at the `go` branch head, and updates `mono`'s `AGENTS.md`, `scripts/` and `e2e/` once item 19 lands. Until then `mono`'s Rust-specific setup is left as it is.

Neither agent adds what "Not in the core" lists, copies code from the Rust Rookery, Hatchery or the prototype, or writes migrations before the first tag. Reading the old code for behaviour is fine.

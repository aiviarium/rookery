# Working on Rookery

Rookery provisions and manages development machines. It sets up Ubuntu hosts and creates machines through provisioner plugins. The software on a host, coding tools among it, comes from plugins. This file is for coding agents working in this repository.

## Layout

| Path | What it is |
| --- | --- |
| `crates/rookery/` | The `rookery` program, for Linux, macOS and Windows. `main.rs` hands it to `src/cli/`, the command line, which is the same on every system. In `cli/`, `args.rs` is the grammar in clap and `docs.rs` writes the reference page from it. `client.rs` reaches the server through rookery-client, on `/ws` or the server's socket, and `login.rs` signs in. `settings.rs` keeps the server and its token, `ask.rs` asks, `follow.rs` shows progress, `out.rs` prints, and `look.rs` has the colours. On Linux, `src/local/` adds the commands of the server's own machine, behind one `#[cfg(target_os = "linux")]`: `rookery serve`, which starts rookery-server, `rookery server setup-key`, `token` and `secrets rekey` over the socket, `import.rs` for `rookery server import`, and `deploy.rs` for the image. `client_only.rs` takes its place on other systems |
| `crates/rookery-client/` | The client of `rookery serve` for the command line, with no server crates in it, so it builds on macOS and Windows: JSON-RPC on `/ws` over TCP, TLS or a Unix socket, its binary streams with credit, and `rpc()` for `POST /rpc`. `net.rs` is the network under it |
| `crates/rookery-api/` | Types and the operations table shared by every program (`ops.rs` names each operation, its HTTP route and its role). `plugin_contract.rs` checks a plugin package against `schemas/rookery-plugin.schema.json`, for `rookery plugin check` and every install |
| `crates/rookery-core/` | Hosts, profiles, links to hosts, registries and secrets, and plugins. The `Store` in `store/` keeps their state on a `Backend`: the server's tables, which rookery-hosts gives it, or `memory.rs` in tests. `kept.rs` and `plugin_state.rs` write files from it. `kept_profile.rs` is the profile a host keeps of itself and `rookery host adopt`. `runner.rs` holds the locks of subjects, which the server makes leases. `keys/` is the key ring and how Rookery logs in to hosts. `host_type.rs` says what each type of host can do. `provisioner.rs` and `resource.rs` are the provisioners of plugins and what they made. `plugins.rs` reads the plugins, and `eggs.rs` installs, updates and removes them through the `hatchery` library |
| `crates/rookery-setup/` | A host's set-up, with nothing drawn. `fields.rs` is the fields of its profile, and `setup.rs` what changes them and what stops it. `flow.rs` has the questions the command line asks, `machine.rs` the machine a provisioner makes, and `event.rs` the keys a prompt takes |
| `crates/rookery-pty/` | Terminals for sessions |
| `crates/rookery-server/` | `rookery serve`, on the Postgres of `ROOKERY_DATABASE_URL`. `config.rs` reads the `ROOKERY_*` variables. `leader.rs` is the advisory lock: one server per database leads, and the others wait as standbys that answer 503. `edge.rs` serves `/ws`, `/rpc` (`rpc.rs`), `/healthz` and the Unix socket, where the CLI of the same machine is an admin with no token. `shell.rs` serves the shell in `ROOKERY_SHELL_DIR`. `client.rs` calls the socket, for `rookery server setup-key`. `methods()` in `lib.rs` lists every facility's methods |
| `crates/rookery-kernel/` | The core of `rookery serve`. In `methods.rs` each method is registered with a level and the schema of its params. `conn.rs` is a connection on `/ws`, `streams.rs` the binary stream channels with credit, `auth.rs` reads a session or a token |
| `crates/rookery-auth/` | How people sign in, and the `auth.*` methods. `links.rs` has the one-time links of local accounts and `session.rs` the device sessions and their cookie. `oidc.rs` signs in through an identity provider with PKCE |
| `crates/rookery-facilities/` | What plugins and the shell build on, a module for each namespace from `settings.*` to `layout.*`. `storage.*` is key-value with revisions. `access.*` holds the grants on resources and gives the kernel the check of a level on one; `admin.rs` has `admin.*` and `audit.list` |
| `crates/rookery-jobs/` | Jobs and the `JobHandler` trait of the handlers that run them inside the server (`jobs.rs`), with `leases.*`, durable `topics.*`, at-most-once `events.*` and `notify.*` |
| `crates/rookery-hosts/` | Hosts inside `rookery serve`. `backend.rs` keeps rookery-core's state in the server's tables (`rookery.hosts`, `host_access`, `host_keys`, `provisioners`, `resources`, `host_messages`, `host_cursors`, and secrets, settings, registries and key-value entries in the facilities' tables), from a thread of its own (`db.rs`). `locks.rs` makes the locks of subjects leases. `runner.rs` runs Rookery's actions as jobs of rookery-jobs: apply, host actions, provisioning and roll-outs, with their output as log items. `serve/` is the leader's state of the hosts, their links and the handlers of each action, and `methods.rs` registers `hosts.*`, `keys.*`, `provisioners.*`, `resources.*`, `installation.*`, `setup.*` and `plugins.*`. `events.rs` tells `hosts.changed` and takes each host's channel in on a durable topic |
| `crates/rookery-store/` | The server's tables in the schema `rookery` on Postgres, made by the numbered migrations in `migrations/`. `seal.rs` seals secrets and credentials in their rows under the key of `ROOKERY_SECRET_KEY`, which `rookery server secrets rekey` changes. `testing.rs` makes a fresh database for each test |
| `crates/rookery-proto/` | The wire types of `rookery serve`, from the JSON-RPC envelope and its errors to the params and results of each method |
| `api/` | `methods.json`, the report of every method of `rookery serve` with its level and params. A test fails while it is stale |
| `client/src/generated/` | The TypeScript types of the wire, written from `rookery-proto`. A test fails while they are stale |
| `crates/rookeryd/`, `crates/box/` | The controller on each host and the agent's own tool there |
| `crates/vendor/` | The vendored `tui-input` crate |
| `provision/` | The installer side of the provisioning source: the role in `ansible/`, first-boot provisioning in `bootstrap/`, the builder of seeds and releases in `bin/rookery.py` and the example profile in `config/`. Rookery names these paths in `layout` in `crates/rookery-core/src/paths.rs` |
| `plugins/` | The plugins that come with Rookery (`backup`, `docker`, `github`, `incus` and `unity`), a static Hatchery registry named `rookery`. Each package in `plugins/rookery/ID/` has a `package.kdl` and its files in `rookery/`, where `rookery.json` follows `schemas/rookery-plugin.schema.json` and `guide.md` is its guide, beside its egg and `index.json`. `builtin.rs` in rookery-core reads the registry. With `provision/` they make the provisioning source, which Rookery carries and unpacks with the same layout |
| `schemas/` | The JSON Schema of a plugin's `rookery.json`, the contract a plugin keeps |
| `recipes/` | Set-ups that go with Rookery but are not installed by it. Each is a folder with a guide and a script. `recipes/README.md` lists them |
| `.agents/skills/` | Skills for coding agents that work with Rookery, such as installing it. `.claude/skills` links here |
| `container/` | The desktop image and Rookery's own image |
| `ci/` | The images and scripts that run the checks and the end-to-end tests in containers |
| `client/` | The `@rookery/client` TypeScript package. It carries the browser `/ws` transport with JSON-RPC 2.0, binary streams with credit and reconnect. It also provides device-session auth helpers, the content-hashed plugin client-half loader, routing and SolidJS bindings. `client/src/generated/` holds the wire types for `rookery-proto`. Rookery ships no UI and has no Nocturne dependency |
| `sdk/` | The `@rookery/sdk` TypeScript package. It defines `defineClient` for client halves, `definePlugin` for host halves and shared data, filter and error helpers |
| `docs/` | The documentation site (Fumadocs). It follows aiviarium/docs-base at commit `dcffeac`, the docs base of the Aiviarium projects. `docs/AGENTS.md` says what Rookery adds to the base and how to take a later version of it |
| `tests/` | Python tests of the provisioning source |

## Skills

Skills for agents are in `.agents/skills/`, each a folder with a `SKILL.md`. `.claude/skills` is a link to the same folder.

| Skill | Use it when |
| --- | --- |
| `install-rookery` | Someone asks you to install or set up Rookery, on their own machine or on a server |

A new skill gets its own folder and a row here and in `.agents/skills/README.md`. Its text has to be true to the docs and the recipes' guides, and it has to pass vale.

## Installing Rookery for someone

- Follow the `install-rookery` skill. It says which recipe in `recipes/` fits and gives its command.
- A recipe that erases disks needs the user's word first, for that server and those disks.
- Do not write an enrollment key or a password into a file in the repository.
- When a Rookery server is installed, give the user its address and a setup key from `rookery server setup-key`. Do not read or print the token in `serve.token`.

## Build and check

Use cargo for everything Rust and bun for `client/` and `sdk/`. These are what CI runs:

```bash
cargo test --locked --no-fail-fast
cargo fmt --check -p rookery-api -p rookery-pty -p rookery-core -p rookery-setup -p rookery -p rookeryd -p box \
  -p rookery-proto -p rookery-store -p rookery-kernel -p rookery-auth -p rookery-server \
  -p rookery-facilities -p rookery-jobs -p rookery-hosts
cargo test --locked -q -p rookery-proto --features ts --test bindings
cargo clippy --locked --all-targets -- -D warnings
cargo run --locked -q -p rookery -- plugin check plugins
python3 -m unittest discover -s tests -v
bun install --frozen-lockfile && bun run check
vale --minAlertLevel=warning --glob='!docs/content/docs/api/{host,serve}/**' README.md AGENTS.md docs/AGENTS.md docs/content/docs plugins/rookery/*/rookery/guide.md
```

`bun run check` runs Biome lint, TypeScript typechecks and Bun tests across `client/` and `sdk/`.

The Docs workflow checks the docs site with `npm run typecheck`, `npm run lint` and `npm run links` in `docs/`, and builds it. It runs for every pull request, as `ci/plan.py` leaves the site out of CI's units.

On macOS and Windows Rookery is the client alone. `rookery serve` on Postgres and every crate it needs (sqlx among them) are Linux-only dependencies of `rookery`, and never reach rookeryd or box. Check that it still builds there with `cargo check -p rookery --target x86_64-pc-windows-msvc` or `--target aarch64-apple-darwin`. The release builds it on those systems.

The tests of `rookery serve` (`rookery-store`, `rookery-server`, `rookery-hosts` and the facilities) need Postgres. Each makes a database of its own from a template, so one server serves every test at once. Without `ROOKERY_TEST_POSTGRES` they are skipped. In CI they fail instead. CI has a Postgres service; on a machine of your own Docker gives one:

```bash
docker run -d --name rookery-test-pg -e POSTGRES_USER=rookery -e POSTGRES_PASSWORD=rookery-test \
  -p 127.0.0.1:55432:5432 postgres:17-alpine -c fsync=off
export ROOKERY_TEST_POSTGRES=postgres://rookery:rookery-test@127.0.0.1:55432/rookery
cargo nextest run -p rookery-proto -p rookery-store -p rookery-kernel -p rookery-auth -p rookery-server \
  -p rookery-facilities -p rookery-jobs -p rookery-hosts
```

The tests of `rookery-hosts` also start `target/debug/rookeryd --single`, which a build of the workspace makes, and are skipped without it.

The tests of a facility call its methods on a kernel of their own (`rookery_kernel::testing`, feature `testing`), with no server in between. A test that hangs is ended after two minutes (`.config/nextest.toml`).

Development and test builds embed quick builds of rookeryd and box (the `host-dev` profile). Only `--release` embeds the release builds. The Python tests skip their YAML checks without PyYAML, so install `provision/requirements.txt` in a virtual environment to run them all.

### Fast local loop

Run Rust tests locally with [cargo-nextest](https://nexte.st), which runs test binaries in parallel (~14s warm vs ~41s with `cargo test`). For one crate that depends on `rookery-api`, pass `-E 'package(<crate>)'` rather than `-p`. Passing `-p` resolves `rookery-api` features differently and triggers a rebuild of roughly 20s:

```bash
cargo nextest run --locked -p rookery-api
cargo nextest run --locked -E 'package(rookery-core)'
TMPDIR=~/.tmp BOX_DESKTOP_ENV=1 cargo nextest run --locked --no-fail-fast
```

Set `TMPDIR` to a short directory on disk, such as `~/.tmp` (`mkdir -p ~/.tmp`), when your checkout sits under a deep path, so Unix domain sockets stay below Linux's 108-byte `SUN_LEN` limit. Don't use `/tmp`: on some machines it is in memory, and test runs fill it. Set `BOX_DESKTOP_ENV=1` inside an `agent-env` container so `crates/box/tests/desktop.rs` uses the test `PATH` instead of re-executing `/usr/local/bin/agent-env`.

Local builds keep Cargo's incremental compilation enabled and emit line tables for Rookery's crates (`debug = "line-tables-only"` in `Cargo.toml`). CI turns incremental builds off (`CARGO_INCREMENTAL=0`) and sets `CARGO_PROFILE_DEV_DEBUG=0` to skip debug information. Store tests in `on_postgres` skip locally unless `ROOKERY_TEST_POSTGRES` points at a PostgreSQL database (CI runs them against a `postgres:17-alpine` service container).

Measured on an 8-core Linux laptop with a warm `target/` directory:

| Check | Time |
| --- | --- |
| Single leaf crate (`-p rookery-api`) | ~0.5s |
| Single library crate (`-E 'package(rookery-core)'`) | ~3.6s |
| Full Rust `nextest` suite | ~14s |
| Full Rust `cargo test` suite | ~41s |
| Incremental `cargo clippy --all-targets` | ~0.3s |
| Formatting and plugin check (`fmt`, `plugin-check`) | ~1.2s |
| Python unit tests (`ci/checks.sh python-tests`) | ~13s |
| Shell syntax (`ci/checks.sh shell-syntax`) | <0.1s |

### In containers

With only Docker or Podman, `ci/run.sh` runs the same checks in an image that has the toolchain. CI uses it too.

```bash
ci/run.sh check [CHECK…]   # fmt, shell-syntax, ansible-syntax, vale, python-tests, rust-tests, plugin-check, clippy
ci/run.sh e2e [TEST…]      # the end-to-end tests, such as e2e_serve.Grants
ci/run.sh e2e --shard NAME # the tests of one shard on fresh hosts, as a CI job runs them
ci/run.sh shell            # a shell in the dev container
ci/run.sh boot cloud|iso   # real installs in QEMU, with KVM; slow
```

The end-to-end tests in `tests/e2e/` use throw-away containers over SSH, with Rookery, rookeryd and `rookery serve`. They run on a network with nothing else on it, in scratch directories. Each scratch workstation runs a `rookery serve` of its own, on a database of its own in the Postgres of `ci/compose.yml`, and its commands reach it on its socket. There are two test hosts, `studio` and `forge`, and the two run in parallel. The modules need different amounts of that setup:

| Tests | Need |
| --- | --- |
| `e2e_access` | A bare host with only SSH, which they leave as they found it |
| `e2e_controller.Controller` | The same host and Rookery's controller alone, with one playbook run |
| `e2e_provisioners` | A provisioner of a made-up plugin, and no test host. With Docker, `ci/run.sh` starts an engine on the test network for the Docker plugin's cases |
| `e2e_restore` | The host `studio` as it is, and a server of its own, restored from a dump of its database. It leaves the host as it is |
| `e2e_node` | The bare host `forge` applied as a node, with a made-up plugin's feature that offers a provisioner. It runs last there and leaves the node |
| The rest | A full apply of the profile |

That engine starts with Docker when the run is complete, names `e2e_provisioners` or is the shard that holds it. `ROOKERY_CI_DOCKER` (`1` or `0`) forces it on or off, and `ROOKERY_CI_DIND_IMAGES` lists images of your own engine to load into it, as it cannot pull. The header of `ci/run.sh` has the rest.

`tests/e2e/run.py` says which test cases run on which host and in what order. Test cases that look at the same state of a host follow each other, so the state is applied once (see the shared states in `tests/e2e/harness.py`). A run ends with the time of each test and test case and the number of playbook runs. The test hosts' logs from a failed run are in `build/ci/logs/`.

CI runs the checks on the runner itself, each in a job of its own, and the end-to-end tests as shards. A shard is a part of the lists that keeps the test cases of each shared state together (`SHARDS` in `tests/e2e/run.py`); one may hold the tests of both hosts, which then run side by side, so that a run asks the runners for fewer jobs. Each runs in a job of its own on fresh hosts, where its first test case makes the state it needs. A test case in no list, or in no shard, stops the run before it starts.

`ci/plan.py` decides what a run does. Each unit has a key, a hash of the files it depends on and of the week. A unit that passed leaves a marker under its key in GitHub's cache, and a later run with the same files skips it. The lint job always runs.

| Unit | Runs for a change to |
| --- | --- |
| `rust-tests`, `clippy` | anything but the docs site, the recipes, `client/` (but its `src/generated/`), `sdk/` and the other workflows |
| `python-tests` | anything but the Rust code, `api/`, `client/`, `sdk/` and the docs site |
| `ts` | `client/`, `sdk/` and the root TypeScript configuration files |
| The shards `studio` and `forge` | anything Rookery is built from but `rookery serve` on Postgres (`SERVER` in `ci/plan.py`), which they do not reach yet: they apply only once |
| The other shards | the role, the plugins, the end-to-end tests, or the Rust code in `SEAM` of `ci/plan.py` (on main: anything Rookery is built from) |

`SEAM` is the Rust code that decides what the role is given or finds on a host. The test `ansible_contract` in `crates/rookery-core/src/apply.rs` compares the variables of made-up profiles with the files in `crates/rookery-core/tests/fixtures/ansible/`, which are in `SEAM` too. A change elsewhere that alters them makes the test fail, and writing them again runs the shards. Code that moves into a new file that feeds the role belongs in `SEAM`.

The Rust jobs restore the target directory from main's cache. `ci/mtimes.py` sets the files that did not change since that build to a time long past, so cargo builds only what changed. Only main saves the caches. Started by hand, CI runs everything.

`ci/run.sh boot` boots Rookery's cloud seed and its USB installer in QEMU, lets the first boot set the host up, and then runs `tests/e2e/boot_*.py` and part of the container suite against it. Each takes 25 to 50 minutes on GitHub's runners, so Boot runs on a nightly schedule (cloud nightly, installer weekly on Mondays) and by hand rather than gating pull requests or pushes to `main`. Run it locally when a change touches provisioning, the role or first boot. Logs, timings and disk use are in `build/boot/`.

## API changes

- An operation of rookeryd is added to `crates/rookery-api/src/ops.rs` with the level it needs. A test fails when a route has no operation or the other way round.
- A method of `rookery serve` is added in the `register` of its facility with `methods.add(…).summary(…).level(…)`, and its params and result types in `rookery-proto`. A method without a level is refused to every caller, and a test fails for it. `ROOKERY_UPDATE_DOCS=1 cargo test -p rookery-server` writes `api/methods.json` again.
- `ROOKERY_UPDATE_DOCS=1 cargo test -p rookery-proto --features ts --test bindings` writes the TypeScript types in `client/src/generated/`. A wire type is added to the list in `crates/rookery-proto/tests/bindings.rs`.
- Work that runs inside the server, such as an apply, is a `JobHandler` given to `rookery_jobs::register` in `methods_with` of `rookery-server`. One that is not safe to repeat says so with `repeatable`: its job gets one attempt, and one the server stopped in fails for a person to run again.
- A migration of the store is a new numbered file in `crates/rookery-store/migrations/` and an entry in `MIGRATIONS` of `migrate.rs`. A migration that ran is never edited: the server refuses to start on one that changed.
- rookeryd's API documents in `docs/openapi/` are committed. Regenerate them with `ROOKERY_UPDATE_DOCS=1 cargo test -p rookery -p rookeryd`. The same writes the command line's reference page, `docs/content/docs/(guide)/reference/cli.mdx`, from the grammar in `crates/rookery/src/cli/args.rs`. A test fails while either is stale.
- The pages under `docs/content/docs/api/host` and `api/server` are generated from those documents and `api/methods.json` when the docs are built (`docs/scripts/reference.mjs`), and are not committed. Do not edit them by hand.

### Actions

Give a new thing a host, the installation, a provisioner or a resource can do an action in the matching registry. It does not get a route of its own. The routes `hosts.act`, `installation.act`, `provisioners.act` and `resources.act` run it, and each action says whether it can run now and what role it needs.

| Subject | Where its actions are |
| --- | --- |
| A host | `HOST` in `crates/rookery-hosts/src/serve/actions.rs`, besides the actions its controller lists |
| The installation | `INSTALLATION` in the same file |
| A provisioner | `provisioner::actions` in `crates/rookery-core/src/provisioner.rs`: its plugin's actions and Rookery's own |
| A resource | `resource::actions` in `crates/rookery-core/src/resource.rs`: those of its kind and its plugin's |

An action that changes something runs as a job of rookery-jobs through `runner.rs` of rookery-hosts. The command line does everything through the API. What works only with the server's own machine, such as writing a USB drive, is an action for a caller there alone (`Caller::Local`, the socket). To one over the network it says why not.

## Versions and releases

- Every program says `rookery_api::VERSION`. Do not read `CARGO_PKG_VERSION` anywhere else. A test checks.
- A release is a pushed tag `vX.Y.Z`. The Build workflow builds with `ROOKERY_BUILD_VERSION` set from it, publishes the release and the image, then opens a pull request that sets the version on `main` (`Cargo.toml`, `Cargo.lock`, `docs/openapi/`). Merge it when its checks have passed. Do not change the version by hand.
- A tag with a suffix, such as `v0.4.0-rc.1`, makes a pre-release and leaves `main` alone.
- A plugin in `plugins/rookery/` is published in the registry there. A change to one gives it a new version in its `package.kdl`, because a published version keeps its contents. `ROOKERY_UPDATE_DOCS=1 cargo test -p rookery-core builtin` writes its egg and `index.json` again, and the test fails while they are stale.

## Conventions

- Commits follow Conventional Commits (`feat(serve): …`, `fix(role): …`, `docs: …`, `ci: …`). Write a short subject, and add a body only when it explains something the diff does not.
- A comment gives the reason for the code in a sentence, and neither code nor docs refer to plans or decisions made elsewhere.
- Documentation is written for people, in short sentences and with tables for lookups. It has to pass vale.
- Tests and examples use made-up hosts such as `studio` and addresses from `192.0.2.0/24`, never a real machine or a real configuration.
- A test that starts `rookery serve` or `rookeryd` sets `XDG_RUNTIME_DIR`, `XDG_CONFIG_HOME` and `XDG_DATA_HOME` to scratch directories, so it cannot touch the user's own.
- Secrets never go into argv, logs or the repository. They travel in private files or on standard input.
- An installation's state goes through the store (`rookery_core::store`), not through files of its own under the configuration or data directory, so that the database and its key file restore it. A file that has to stay, such as `id_ed25519` or `serve.token`, is written from the store.

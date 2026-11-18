# Working on Rookery

Rookery provisions and manages development machines. It sets up Ubuntu hosts and creates machines through provisioner plugins. The software on a host, coding tools among it, comes from plugins. This file is for coding agents working in this repository.

## Layout

| Path | What it is |
| --- | --- |
| `crates/rookery/` | The `rookery` program, for Linux, macOS and Windows. `main.rs` opens the screens, and `src/cli/` is the command line, the same on every system. In `cli/`, `args.rs` is the grammar in clap and `docs.rs` writes the reference page from it. `client.rs` reaches the server. `ask.rs` asks, `follow.rs` shows progress, and `out.rs` prints. On Linux, `src/local/` adds the commands of the server's own machine and `rookery serve` (`src/local/serve/`), behind one `#[cfg(target_os = "linux")]`. `client_only.rs` takes its place on other systems. In `serve/`, `here.rs` is the server in Rookery's own process. The screens and commands use it when no other is set. Then `actions.rs` has the actions of hosts and of the installation, `keys.rs` the key ring's routes, `provisioners.rs` and `resources.rs` those of provisioners and resources |
| `crates/rookery-api/` | Types and the operations table shared by every program (`ops.rs` names each operation, its HTTP route and its role). `plugin_contract.rs` checks a plugin package against `schemas/rookery-plugin.schema.json`, for `rookery plugin check` and every install |
| `crates/rookery-core/` | Hosts, profiles, links to hosts, registries and secrets, and plugins. The store is in `store/`. `kept.rs` and `plugin_state.rs` write files from it. `kept_profile.rs` is the profile a host keeps of itself and `rookery host adopt`. `runner.rs` runs every action as a job. `keys/` is the key ring and how Rookery logs in to hosts. `host_type.rs` says what each type of host can do. `provisioner.rs` and `resource.rs` are the provisioners of plugins and what they made. `simulate.rs` is the world of `rookery serve --simulate`: its hosts, its provisioner `sim/cloud` and its seed. `plugins.rs` reads the plugins, and `eggs.rs` installs, updates and removes them through the `hatchery` library |
| `crates/rookery-tui/` | The parts of the screens with no terminal in them. `fields.rs`, `setup.rs` and `flow.rs` describe a host's set-up once, for the screens' tabs and the command line's prompts and flags alike, and `theme.rs` has the colours both draw with |
| `crates/rookery-pty/` | Terminals for sessions |
| `crates/rookeryd/`, `crates/box/` | The controller on each host and the agent's own tool there |
| `crates/rookery-page/` | Rookery's screens in a terminal. They do everything through the API of a server. That is one elsewhere, a `rookery serve` on its socket, or the one in the process. `net.rs` reaches it, `portal.rs` is its portal, `settings.rs` keeps the server and its token, and `device.rs` is what the program starts them with |
| `crates/vendor/` | The vendored `tui-input` crate |
| `provision/` | The installer side of the provisioning source: the role in `ansible/`, first-boot provisioning in `bootstrap/`, the builder of seeds and releases in `bin/rookery.py` and the example profile in `config/`. Rookery names these paths in `layout` in `crates/rookery-core/src/paths.rs` |
| `plugins/` | The plugins that come with Rookery (`backup`, `docker`, `github`, `incus` and `unity`), a static Hatchery registry named `rookery`. Each package in `plugins/rookery/ID/` has a `package.kdl` and its files in `rookery/`, where `rookery.json` follows `schemas/rookery-plugin.schema.json` and `guide.md` is its guide, beside its egg and `index.json`. `builtin.rs` in rookery-core reads the registry. With `provision/` they make the provisioning source, which Rookery carries and unpacks with the same layout |
| `schemas/` | The JSON Schema of a plugin's `rookery.json`, the contract a plugin keeps |
| `recipes/` | Set-ups that go with Rookery but are not installed by it. Each is a folder with a guide and a script. `recipes/README.md` lists them |
| `.agents/skills/` | Skills for coding agents that work with Rookery, such as installing it. `.claude/skills` links here |
| `container/` | The desktop image and Rookery's own image |
| `ci/` | The images and scripts that run the checks and the end-to-end tests in containers |
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

Use cargo for everything Rust. These are what CI runs:

```bash
cargo test --locked
cargo fmt --check -p rookery-api -p rookery-pty -p rookery-core -p rookery-tui -p rookery -p rookeryd -p box -p rookery-page
cargo clippy --locked --all-targets -- -D warnings
cargo run --locked -q -p rookery -- plugin check plugins
python3 -m unittest discover -s tests -v
vale --minAlertLevel=warning --glob='!docs/content/docs/api/{host,serve}/**' README.md AGENTS.md docs/AGENTS.md docs/content/docs plugins/rookery/*/rookery/guide.md
```

The Docs workflow checks the docs site with `npm run typecheck`, `npm run lint` and `npm run links` in `docs/`, and builds it. It runs for every pull request, as `ci/plan.py` leaves the site out of CI's units.

On macOS and Windows Rookery is the client alone. Check that it still builds there with `cargo check -p rookery --target x86_64-pc-windows-msvc` or `--target aarch64-apple-darwin`. The release builds it on those systems.

Development and test builds embed quick builds of rookeryd and box (the `host-dev` profile). Only `--release` embeds the release builds. The Python tests skip their YAML checks without PyYAML, so install `provision/requirements.txt` in a virtual environment to run them all.

### In containers

With only Docker or Podman, `ci/run.sh` runs the same checks in an image that has the toolchain. CI uses it too.

```bash
ci/run.sh check [CHECK…]   # fmt, shell-syntax, ansible-syntax, vale, python-tests, rust-tests, plugin-check, clippy
ci/run.sh e2e [TEST…]      # the end-to-end tests, such as e2e_serve.Roles
ci/run.sh e2e --shard NAME # the tests of one shard on a fresh host, as a CI job runs them
ci/run.sh shell            # a shell in the dev container
ci/run.sh boot cloud|iso   # real installs in QEMU, with KVM; slow
```

The end-to-end tests in `tests/e2e/` use throw-away containers over SSH, with Rookery, rookeryd and `rookery serve`. They run on a network with nothing else on it, in scratch directories. There are two test hosts, `studio` and `forge`, and the two run in parallel. The modules need different amounts of that setup:

| Tests | Need |
| --- | --- |
| `e2e_access` | A bare host with only SSH, which they leave as they found it |
| `e2e_controller.Controller` | The same host and Rookery's controller alone, with one playbook run |
| `e2e_provisioners` | A provisioner of a made-up plugin, and no test host. With Docker, `ci/run.sh` starts an engine on the test network for the Docker plugin's cases |
| `e2e_restore` | The host `studio` as it is, and a server of its own with an encrypted store. It leaves the host as it is |
| `e2e_node` | The bare host `forge` applied as a node, with a made-up plugin's feature that offers a provisioner. It runs last there and leaves the node |
| The rest | A full apply of the profile |

That engine starts with Docker when the run is complete, names `e2e_provisioners` or is the shard that holds it. `ROOKERY_CI_DOCKER` (`1` or `0`) forces it on or off, and `ROOKERY_CI_DIND_IMAGES` lists images of your own engine to load into it, as it cannot pull. The header of `ci/run.sh` has the rest.

`tests/e2e/run.py` says which test cases run on which host and in what order. Test cases that look at the same state of a host follow each other, so the state is applied once (see the shared states in `tests/e2e/harness.py`). A run ends with the time of each test and test case and the number of playbook runs. The test hosts' logs from a failed run are in `build/ci/logs/`.

CI runs the checks on the runner itself, each in a job of its own, and the end-to-end tests as shards. A shard is a part of a host's list that keeps the test cases of each shared state together (`SHARDS` in `tests/e2e/run.py`). Each runs in a job of its own on a fresh host, where its first test case makes the state it needs. A test case in no list, or in no shard, stops the run before it starts.

`ci/plan.py` decides what a run does. Each unit has a key, a hash of the files it depends on and of the week. A unit that passed leaves a marker under its key in GitHub's cache, and a later run with the same files skips it. The lint job always runs.

| Unit | Runs for a change to |
| --- | --- |
| `rust-tests`, `clippy` | anything but the docs site, the recipes and the other workflows |
| `python-tests` | anything but the Rust code and the docs site |
| The shards `studio` and `forge` | anything Rookery is built from: they apply only once |
| The other shards | the role, the plugins, the end-to-end tests, or the Rust code in `SEAM` of `ci/plan.py` (on main: anything Rookery is built from) |

`SEAM` is the Rust code that decides what the role is given or finds on a host. The test `ansible_contract` in `crates/rookery-core/src/apply.rs` compares the variables of made-up profiles with the files in `crates/rookery-core/tests/fixtures/ansible/`, which are in `SEAM` too. A change elsewhere that alters them makes the test fail, and writing them again runs the shards. Code that moves into a new file that feeds the role belongs in `SEAM`.

The Rust jobs restore the target directory from main's cache. `ci/mtimes.py` sets the files that did not change since that build to a time long past, so cargo builds only what changed. Only main saves the caches. Started by hand, CI runs everything.

`ci/run.sh boot` boots Rookery's cloud seed and its USB installer in QEMU, lets the first boot set the host up, and then runs `tests/e2e/boot_*.py` and part of the container suite against it. Each takes 10 to 15 minutes on a machine with KVM, so run it when a change touches provisioning, the role or first boot. CI boots only the cloud image for such a change. The installer is booted once a week and when the Boot workflow is started by hand. CI runs both for changes to provisioning (the Boot workflow, on a hosted runner with KVM). Logs, timings and disk use are in `build/boot/`.

## API changes

- Add the operation to `crates/rookery-api/src/ops.rs` with the role it needs. A test fails when a route has no operation or the other way round.
- The API documents in `docs/openapi/` are committed. Regenerate them with `ROOKERY_UPDATE_DOCS=1 cargo test -p rookery -p rookeryd`. The same writes the command line's reference page, `docs/content/docs/(guide)/reference/cli.mdx`, from the grammar in `crates/rookery/src/cli/args.rs`. A test fails while either is stale.
- The pages under `docs/content/docs/api/host` and `api/serve` are generated from those documents when the docs are built (`docs/scripts/reference.mjs`), and are not committed. Do not edit them by hand.
- The screenshots of the docs are taken offscreen by `docs/scripts/screens.py`, from made-up hosts.

### Actions

Give a new thing a host, the installation, a provisioner or a resource can do an action in the matching registry. It does not get a route of its own. The routes `hosts.act`, `installation.act`, `provisioners.act` and `resources.act` run it, and each action says whether it can run now and what role it needs.

| Subject | Where its actions are |
| --- | --- |
| A host | `HOST` in `crates/rookery/src/local/serve/actions.rs`, besides the actions its controller lists |
| The installation | `INSTALLATION` in the same file |
| A provisioner | `provisioner::actions` in `crates/rookery-core/src/provisioner.rs`: its plugin's actions and Rookery's own |
| A resource | `resource::actions` in `crates/rookery-core/src/resource.rs`: those of its kind and its plugin's |

An action that changes something runs as a job through `runner.rs`. The screens do everything through the API. What works only with the server's own machine, such as writing a USB drive, is an action for a caller there alone (`Seen::local` is the socket or the process). To one over the network it says why not.

## Versions and releases

- Every program says `rookery_api::VERSION`. Do not read `CARGO_PKG_VERSION` anywhere else. A test checks.
- A release is a pushed tag `vX.Y.Z`. The Build workflow builds with `ROOKERY_BUILD_VERSION` set from it, publishes the release and the image, then opens a pull request that sets the version on `main` (`Cargo.toml`, `Cargo.lock`, `docs/openapi/`). Merge it when its checks have passed. Do not change the version by hand.
- A tag with a suffix, such as `v0.4.0-rc.1`, makes a pre-release and leaves `main` alone.
- A plugin in `plugins/rookery/` is published in the registry there. A change to one gives it a new version in its `package.kdl`, because a published version keeps its contents. `ROOKERY_UPDATE_DOCS=1 cargo test -p rookery-core builtin` writes its egg and `index.json` again, and the test fails while they are stale.

## Conventions

- Commits follow Conventional Commits (`feat(serve): …`, `fix(role): …`, `docs: …`, `ci: …`). Write a short subject, and add a body only when it explains something the diff does not.
- A comment gives the reason for the code in a sentence, and neither code nor docs refer to plans or decisions made elsewhere.
- Documentation is written for people, in short sentences and with tables for lookups. It has to pass vale.
- Tests, screenshots and examples use made-up hosts such as `studio` and addresses from `192.0.2.0/24`, never a real machine or a real configuration.
- A test that starts `rookery serve` or `rookeryd` sets `XDG_RUNTIME_DIR`, `XDG_CONFIG_HOME` and `XDG_DATA_HOME` to scratch directories, so it cannot touch the user's own.
- Secrets never go into argv, logs or the repository. They travel in private files or on standard input.
- An installation's state goes through the store (`rookery_core::store`), not through files of its own under the configuration or data directory, so that the database and its key file restore it. A file that has to stay, such as `id_ed25519` or `serve.token`, is written from the store.

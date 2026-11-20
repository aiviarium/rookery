# Rookery is a Go workspace: the core module here, a module per first-party
# plugin under plugins/. Targets run from this directory.
GO ?= go
GOBIN ?= $(HOME)/go/bin
export PATH := $(GOBIN):$(PATH)

# Unix sockets in tests live in a short directory on disk, and TMPDIR is where
# t.TempDir() and os.MkdirTemp put it. Never /tmp.
export TMPDIR ?= $(HOME)/.tmp

# The generation and lint tools, pinned. `make tools` installs them into GOBIN.
BUF_VERSION ?= v1.47.2
SQLC_VERSION ?= v1.27.0
PROTOC_GEN_GO_VERSION ?= v1.36.0
PROTOC_GEN_CONNECT_GO_VERSION ?= v1.18.1
GOLANGCI_LINT_VERSION ?= v2.15.0

# The dev database, and the one `storetest` clones from.
PG_IMAGE ?= pgvector/pgvector:pg17
PG_NAME ?= rookery-db
PG_PORT ?= 55432
PG_USER ?= rookery
PG_PASSWORD ?= rookery-test
DATABASE_URL ?= postgres://$(PG_USER):$(PG_PASSWORD)@127.0.0.1:$(PG_PORT)/rookery?sslmode=disable
TEST_POSTGRES ?= postgres://$(PG_USER):$(PG_PASSWORD)@127.0.0.1:$(PG_PORT)/postgres?sslmode=disable

# Every plugin module in the workspace.
PLUGIN_MODULES := $(patsubst %/go.mod,%,$(wildcard plugins/*/go.mod))

# The TypeScript workspace: the SDK and the shell, one lockfile.
WEB_DIR ?= web/shell

.PHONY: all tools tools-go lint-go test-shard tools-ts gen gen-proto gen-sql gen-schema db reset test test-plugins lint fmt ts web web-test e2e dev build cross clean ansible

all: gen lint test ts web

tools: tools-go
	$(MAKE) tools-ts

# The Go tools alone, for the CI jobs that need no TypeScript.
tools-go:
	$(GO) install github.com/bufbuild/buf/cmd/buf@$(BUF_VERSION)
	$(GO) install github.com/sqlc-dev/sqlc/cmd/sqlc@$(SQLC_VERSION)
	$(GO) install google.golang.org/protobuf/cmd/protoc-gen-go@$(PROTOC_GEN_GO_VERSION)
	$(GO) install connectrpc.com/connect/cmd/protoc-gen-connect-go@$(PROTOC_GEN_CONNECT_GO_VERSION)
	$(GO) install github.com/golangci/golangci-lint/v2/cmd/golangci-lint@$(GOLANGCI_LINT_VERSION)

# The TypeScript tools: protoc-gen-es, which buf generate runs, and the type
# checker and test runner from the workspace's dependencies.
tools-ts:
	@if [ -f package.json ]; then \
		bun install --frozen-lockfile; \
	else echo "ts: no workspace yet, skipped"; fi

gen: gen-proto gen-sql gen-schema

gen-proto:
	@if [ -f package.json ] && [ ! -x node_modules/.bin/protoc-gen-es ]; then \
		$(MAKE) tools-ts; fi
	buf generate

gen-sql:
	@if [ -f internal/store/schema.sql ]; then sqlc generate; else echo "sqlc: no schema yet, skipped"; fi

# The manifest's JSON Schema, from the Go types in internal/plugins.
gen-schema:
	$(GO) run ./cmd/pluginschema -o schemas/plugin.schema.json

db:
	@if bash -c 'exec 3<>/dev/tcp/127.0.0.1/$(PG_PORT)' 2>/dev/null; then \
		echo "db: a Postgres already answers on 127.0.0.1:$(PG_PORT)"; \
		exit 0; \
	fi; \
	if docker inspect $(PG_NAME) >/dev/null 2>&1; then \
		docker start $(PG_NAME) >/dev/null; \
	else \
		docker run -d --name $(PG_NAME) \
			-e POSTGRES_USER=$(PG_USER) \
			-e POSTGRES_PASSWORD=$(PG_PASSWORD) \
			-e POSTGRES_DB=rookery \
			-p 127.0.0.1:$(PG_PORT):5432 \
			$(PG_IMAGE) -c fsync=off >/dev/null; \
	fi; \
	for i in $$(seq 1 60); do \
		docker exec $(PG_NAME) pg_isready -U $(PG_USER) >/dev/null 2>&1 && break; \
		sleep 1; \
	done
	@echo "export ROOKERY_TEST_POSTGRES=$(TEST_POSTGRES)"

reset:
	ROOKERY_DATABASE_URL=$(DATABASE_URL) $(GO) run ./cmd/rookery server reset

test:
	@mkdir -p $(TMPDIR)
	$(GO) test -race ./...

# One of SHARDS slices of the core module's packages, for CI's parallel test
# jobs: make test-shard SHARD=0 SHARDS=2.
SHARD ?= 0
SHARDS ?= 1
test-shard:
	@mkdir -p $(TMPDIR)
	$(GO) test -race $$($(GO) list ./... | awk 'NR % $(SHARDS) == $(SHARD)')

# Each plugin module: the Docker one drives the runner's engine, the Incus one a
# fake of its API.
test-plugins:
	@mkdir -p $(TMPDIR)
	@for m in $(PLUGIN_MODULES); do \
		echo "==> $$m"; (cd $$m && $(GO) test -race ./...) || exit 1; \
	done

fmt:
	gofmt -l -w .

lint: lint-go
	@if [ -d ansible ]; then $(MAKE) ansible; fi

# The Go and proto linters, without Ansible.
lint-go:
	@unformatted=$$(gofmt -l .); \
		if [ -n "$$unformatted" ]; then echo "gofmt found unformatted files:"; echo "$$unformatted"; exit 1; fi
	$(GO) vet ./...
	golangci-lint run
	buf lint
	@for m in $(PLUGIN_MODULES); do \
		echo "==> $$m"; (cd $$m && $(GO) vet ./... && golangci-lint run) || exit 1; \
	done

# The Ansible an apply runs: the roles are linted, and the playbook that runs the
# base role by hand is syntax-checked. An apply renders site.yml.j2 itself.
ansible:
	ansible-lint ansible
	ansible-playbook --syntax-check -i localhost, ansible/base.yml
	ansible-playbook --syntax-check -i localhost, ansible/desktop.yml

# The TypeScript workspace: its dependencies as the lockfile has them, every
# package type-checked under tsc's strict settings, and the unit tests. The
# fixture plugins are workspace members, so their halves are type-checked too,
# and their bundles are built the way an install builds them.
ts:
	@if [ -f package.json ]; then \
		bun install --frozen-lockfile && bun run typecheck && bun run test; \
	else echo "ts: no workspace yet, skipped"; fi
	@for plugin in testdata/plugins/example-ts testdata/plugins/shell; do \
		if [ -f $$plugin/package.json ]; then cd $$plugin && bun run build || exit 1; cd $(CURDIR); fi; \
	done

# The default shell, built into internal/webshell/dist for go:embed. A checkout
# without this build embeds the placeholder page instead.
web:
	@if [ -f $(WEB_DIR)/package.json ]; then bun run web; else echo "web: no $(WEB_DIR) yet, skipped"; fi

# The shell's unit tests alone, without the SDK's.
web-test:
	@if [ -f $(WEB_DIR)/package.json ]; then \
		cd $(WEB_DIR) && bun install --frozen-lockfile && bun run typecheck && bun test; fi

# The browser test starts a real server, so it runs after the shell is built.
e2e: web
	$(GO) test -race -count=1 -tags e2e ./e2e/...

dev: db
	ROOKERY_DATABASE_URL=$(DATABASE_URL) $(GO) run ./cmd/rookery serve

# The release build ships the shell, so it is built first.
build: web
	$(GO) build -o bin/ ./cmd/...

# The binaries the release ships: rookery for every OS, rookeryd and box for the
# Linux hosts Rookery manages.
cross:
	@mkdir -p $(TMPDIR)
	GOOS=darwin GOARCH=arm64 $(GO) build -o /dev/null ./cmd/rookery
	GOOS=windows GOARCH=amd64 $(GO) build -o /dev/null ./cmd/rookery
	CGO_ENABLED=0 GOOS=linux GOARCH=arm64 $(GO) build -o /dev/null ./cmd/rookeryd ./cmd/box

clean:
	rm -rf bin web/shell/dist web/shell/test-results web/shell/playwright-report

# Stage the Linux executables and package the first-party plugin bundles next
# to the server binary for release.
.PHONY: bundle-plugins
bundle-plugins:
	./scripts/bundle-plugins "$(CURDIR)/bin/plugins"

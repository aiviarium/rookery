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

.PHONY: all tools gen gen-proto gen-sql db reset test lint fmt ts e2e dev build cross clean ansible

all: gen lint test

tools:
	$(GO) install github.com/bufbuild/buf/cmd/buf@$(BUF_VERSION)
	$(GO) install github.com/sqlc-dev/sqlc/cmd/sqlc@$(SQLC_VERSION)
	$(GO) install google.golang.org/protobuf/cmd/protoc-gen-go@$(PROTOC_GEN_GO_VERSION)
	$(GO) install connectrpc.com/connect/cmd/protoc-gen-connect-go@$(PROTOC_GEN_CONNECT_GO_VERSION)
	$(GO) install github.com/golangci/golangci-lint/v2/cmd/golangci-lint@$(GOLANGCI_LINT_VERSION)

gen: gen-proto gen-sql gen-schema

gen-proto:
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
	@for m in $(PLUGIN_MODULES); do \
		echo "==> $$m"; (cd $$m && $(GO) test -race ./...) || exit 1; \
	done

fmt:
	gofmt -l -w .

lint:
	@unformatted=$$(gofmt -l .); \
		if [ -n "$$unformatted" ]; then echo "gofmt found unformatted files:"; echo "$$unformatted"; exit 1; fi
	$(GO) vet ./...
	golangci-lint run
	buf lint
	@if [ -d ansible ]; then $(MAKE) ansible; fi
	@for m in $(PLUGIN_MODULES); do \
		echo "==> $$m"; (cd $$m && $(GO) vet ./... && golangci-lint run) || exit 1; \
	done

# The Ansible an apply runs: the roles are linted, and the playbook that runs the
# base role by hand is syntax-checked. An apply renders site.yml.j2 itself.
ansible:
	ansible-lint ansible
	ansible-playbook --syntax-check -i localhost, ansible/base.yml

# The TypeScript SDK arrives with item 17.
ts:
	@if [ -f sdk/ts/package.json ]; then cd sdk/ts && bun install && bun run typecheck && bun test; \
		else echo "ts: no sdk/ts yet, skipped"; fi

# The end-to-end test arrives with item 7.
e2e:
	@if [ -d e2e ]; then $(GO) test -count=1 -tags e2e ./e2e/...; \
		else echo "e2e: no e2e tests yet, skipped"; fi

dev: db
	ROOKERY_DATABASE_URL=$(DATABASE_URL) $(GO) run ./cmd/rookery serve

build:
	$(GO) build -o bin/ ./cmd/...

# The binaries the release ships: rookery for every OS, rookeryd and box for the
# Linux hosts Rookery manages.
cross:
	GOOS=darwin GOARCH=arm64 $(GO) build -o /dev/null ./cmd/rookery
	GOOS=windows GOARCH=amd64 $(GO) build -o /dev/null ./cmd/rookery
	CGO_ENABLED=0 GOOS=linux GOARCH=arm64 $(GO) build -o /dev/null ./cmd/rookeryd ./cmd/box

clean:
	rm -rf bin

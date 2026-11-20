# Rookery's TypeScript SDK

A bun workspace with the browser's contract and the SDK a plugin's client half and
service are written against. The protos are the contract, so its API types are
generated with buf: `make gen` writes `proto/src/gen`, and CI fails while it is
stale.

| Package | Covers |
| --- | --- |
| `@rookery/proto` | The generated messages and service descriptors, and nothing hand-written |
| `@rookery/client` | The edge in a browser: one typed client per service, sign-in, the current space, live queries, live documents, terminals, a host's desktop, the client-half loader |
| `@rookery/client/solid` | SolidJS 2 bindings: signals and async primitives over the client |
| `@rookery/sdk` | `defineClient` for a client half, and the runtime a plugin's service runs on Bun or Node |

```bash
make ts          # bun install --frozen-lockfile, tsc --noEmit, bun test
```

## The browser

```ts
import { createRookery } from "@rookery/client";

const rookery = createRookery();               // the page's own origin
await rookery.signIn.refresh();                // AuthService.Me through the cookie
rookery.setSpace(spaceId);                     // Rookery-Space on every call after
const { records } = await rookery.data.find({ collection: "example/notes" });
```

`RookeryOptions.baseUrl`, `fetch` and `transport` are for a page served from
another address and for tests. `fetch` is wrapped so every call carries cookies,
the CSRF token from the edge's `rookery_csrf` cookie in the `Rookery-CSRF` header,
and the current space in `Rookery-Space`.

A new core service drops in by regeneration: its descriptor arrives in
`@rookery/proto`, and one line in `services` in `src/transport.ts` gives it a typed
client and a name on `createRookery()`. Every service in `proto/rookery/v1` is
there, except the services whose calls are streams a browser cannot open.

### Live queries

`DataService.Live` is a server stream that sends the matching records and then
each change. A subscriber that falls behind is sent `resync` and then the records
that match now; `LiveQuery` keeps a keyed snapshot, replaces it on a resync, and
reconnects with backoff when the stream ends. The query takes a structural
`LiveSource`, which `rookery.data` is.

### The routes that are not Connect

The browser cannot set a header on a WebSocket, so the current space travels in
the query (`?space=…`) and the session cookie authenticates as usual.

| Route | Frames |
| --- | --- |
| `/ws/documents/{id}` | binary: kind byte 1 (`update`, stored and relayed) or 2 (`awareness`, relayed only), then the CRDT's bytes. The server sends the latest snapshot first, as an update |
| `/ws/terminals/{id}` | binary: the PTY's bytes in both directions. text: JSON control, `{"type":"resize","cols":…,"rows":…}` to the server and `{"type":"exit","code":…}` from it |
| `/ws/hosts/{name}/desktop` | binary: the RFB byte stream, unread. `desktop.url(host)` is what noVNC opens itself; `desktop.open(host)` is a socket with the WebSocket interface for a client that takes one |

The document and terminal frames are this client's proposal for item 12's and item
6's routes; when those land, the server follows these bytes or this client is
changed with them.

## Solid

`@rookery/client/solid` is Solid 2 and nothing older:

```tsx
const signIn = useSignIn(rookery);              // who is signed in, as a signal
const notes = useLiveQuery(rookery.data, { collection: "example/notes", space: rookery.space() });
const query = createQuery(() => rookery.data.find({ collection: "example/notes" }));

<Show when={signIn.user()} fallback={<p>Sign in</p>}>{user => <p>{user().name}</p>}</Show>
<For each={notes.records()}>{row => <p>{row.record.title}</p>}</For>
<p>{query.status() === "loading" ? "Loading" : query()?.records.length}</p>
```

`useDocument`, `useTerminal` and `useDesktop` open their route with their owner and
close it when it is disposed; each reads its status, and the terminal its output
and exit code, as signals. `createQuery` keeps the last answer visible while a
refresh is in flight (`pending()`), and drops an answer that lands after its
component is gone.

The client's sources push, so their bindings are signals rather than async memos:
a socket's status and a live query's snapshot are known the moment they arrive,
and a shell draws its own waiting state from `status()` or `query.pending()`.

### Client halves

```ts
import { defineClient } from "@rookery/sdk/client";

export default defineClient({
  views: {
    notes: (context, element) => {
      const { records } = context.rookery.live({ collection: "example/notes" });
      // draw into element; return what removes it
    },
  },
});
```

`loadClientHalves()` asks `PluginService.Contributions`, decodes each plugin's
`contributes_json`, imports its `index.js` from `/plugins/{id}/{hash}/`, and
hands the shell the halves with a context: the typed API, the plugin's id and
contributions, and navigation. A half that fails to load is reported and left
out, so one broken plugin does not keep the shell from starting.

## A plugin's service

The server starts a plugin's service as a process and names two Unix sockets and a
token in its environment, the same variables `pkg/sdk` reads in Go:

| Variable | What it names |
| --- | --- |
| `ROOKERY_SOCKET` | The server's socket, which the service calls |
| `ROOKERY_PLUGIN_SOCKET` | The socket this service serves on |
| `ROOKERY_PLUGIN_TOKEN` | The credential for the server's socket |
| `ROOKERY_SECRETS` | The file a provisioner's secrets are in, when it declared any |

```ts
import { pluginConfig, servePlugin } from "@rookery/sdk";

const config = pluginConfig();                 // refuses to start without the two sockets
const plugin = await servePlugin((router) => {
  // The procedures the manifest declares are registered here.
});
process.on("SIGTERM", () => void plugin.close());
```

On Node the socket serves HTTP/2, which is what the server's gRPC caller needs;
Bun has no HTTP/2 server, so a service on Bun speaks Connect and gRPC-Web over
HTTP/1.1. The plugin service proto (`rookery.plugin.v1.PluginService`, its
procedures and the server's service) is not on `go` yet; `servePlugin` takes the
router those land on, and the rest of the runtime follows it.

## The fixtures

`testdata/plugins/example-ts` and `testdata/plugins/shell` are workspace members,
so `bun install` links them to the packages here and `make ts` type-checks and
builds them:

| Fixture | What it is |
| --- | --- |
| `example-ts` | The manifest `example` has in TypeScript: a collection, a procedure, a job kind, and a client half that draws the notes |
| `shell` | The shell contract as a page: `createRookery`, the contributions of every installed plugin, and a client half's view in the outlet |

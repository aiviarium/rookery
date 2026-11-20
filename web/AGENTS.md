# Working on the default shell

`web/` is Rookery's own UI: the SolidJS 2 app the server serves at `/` while no
installed plugin declares `shell: true`. It is a Bun workspace member beside
`sdk/ts`, with one lockfile at the repository root.

| Path | What it is |
| --- | --- |
| `web/shell/` | `@rookery/shell`, the app: Vite, `@solidjs/vite-plugin`, SolidJS 2, happy-dom tests |
| `web/shell/src/shell.tsx` | The shell's state: the client, sign-in, the space, the connection, notifications and the plugins' halves |
| `web/shell/src/chrome/` | The chrome: sidebar, top bar, account menu, space switcher, notifications, status bar |
| `web/shell/src/pages/` | The shell's own pages, one folder per area, joined by `index.ts` |
| `web/shell/src/plugin/` | The client-half host: a plugin's page and its taps on a host's page |
| `web/shell/src/ui.tsx` | The parts every page draws with |
| `internal/webshell/` | The Go package that embeds `dist/` and serves it for `/` and unmatched paths |

## Commands

```bash
make web        # build into internal/webshell/dist, which the server embeds
make web-test   # the shell's type-check and unit tests alone
make ts         # the whole TypeScript workspace, the shell included
make e2e        # the Go end-to-end test, with the shell's Playwright spec
```

`bun run dev` in `web/shell` serves the app with hot reloading against
`ROOKERY_SERVER`'s origin only if the page and the edge share one; the usual way
to look at it is `make dev` with the server, which serves the last build.

`internal/webshell/dist/index.html` is committed as a placeholder page, so
`go build` never needs Bun. `make web` replaces it with Vite's build and writes
the hashed assets beside it; those are ignored, and the built `index.html` shows
as a change until it is restored. Never commit a built `index.html`.

## Pages: the seam

A page area is a folder under `web/shell/src/pages/` with an `index.ts` that
exports two things:

```ts
// web/shell/src/pages/notes/index.ts
import { defineRoutes } from "@solidjs/router";

import type { Area } from "../types.ts";
import { NotesPage } from "./notes.tsx";

export const routes = defineRoutes([{ path: "/notes", component: NotesPage }]);

export const nav: Area["nav"] = [{ label: "Notes", href: "/notes", group: "Workspace", icon: "document" }];
```

* `routes` is what the area answers, as `@solidjs/router` route definitions. A
  route may nest children, use parameters (`/hosts/:name`) and lazy components.
* `nav` is where the area sits in the sidebar: `label`, `href`, an optional
  `group` (the first group has none) and an optional `icon` from `GlyphName` in
  `src/ui.tsx`.

`web/shell/src/pages/index.ts` lists the areas (`AREAS`) and joins them. Adding
an area is one import and one entry there: no other file changes, and areas
never import each other.

Pages read the shell with `useShell()` from `src/shell.tsx`: `rookery` is the
typed client (`@rookery/client`), `signIn` who is signed in, `spaces` the space
list and the current space, `connection` the events stream, `notifications`, and
`plugins` the installed client halves. A page never builds a transport.

A page that shows data asks for it with `createQuery` from
`@rookery/client/solid`, and asks again when the events stream says it changed:

```tsx
const hosts = createQuery(() => shell.rookery.hosts.list({ limit: 200 }));
onSettled(() => shell.connection.on("rookery/hosts.changed", () => void hosts.refresh()));
```

A page that shows a space's records reads `shell.spaces.version()` and asks
again when it changes.

## The chrome and plugin hosting

The navigation is the areas' `nav` joined with the installed plugins'
`contributes.nav`; each plugin's views and admin pages are routes resolved by
`src/plugin/route.tsx` from `PluginService.Contributions`, and a page is mounted
with `defineClient`'s contract — `mount(context, element)` — through
`src/plugin/mount.tsx`, which also hands a host's page its tabs.

A plugin's client half is loaded from `/plugins/{id}/{hash}/index.js`; a half
that fails to load is reported in the sidebar and left out.

## Tests

`bun test` with happy-dom. `test/transform.ts` compiles JSX with Solid 2's Babel
plugin, `test/fakes.ts` answers the edge's services from a made-up site over
Connect's in-memory transport, and `test/mount.tsx` renders the whole app. Each
test is one page's main interaction, not a matrix.

`web/shell/e2e/` is one Playwright spec against a real `rookery serve`; the Go
test `e2e/shell_test.go` starts the server, makes the first admin's setup link
and runs the spec with `bun x playwright test`. It skips without Bun or
Playwright's Chromium locally and fails in CI.

## Nocturne

Every UI part must come from `@aiviarium/nocturne` and use its tokens and
`nc-*` classes; no literal colours, no forked primitives. Nocturne is pinned by
the commit of its `solid-2` branch in `web/shell/package.json`; inside
`aiviarium/mono` the workspace links the sibling `nocturne/` checkout when it is
built against this one.

Until that branch is published, `src/ui.tsx` draws the parts itself with
Nocturne's `nc-*` classes and tokens. It is the only file to change when the
components arrive: re-export Nocturne's `Button`, `TextField`, `Menu`, `Dialog`
and the rest from it, and the pages keep their imports.

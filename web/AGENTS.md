# Working on the default shell

`web/` is Rookery's own UI: the SolidJS 2 app the server serves at `/` while no
installed plugin declares `shell: true`. It is a Bun workspace member beside
`sdk/ts`, with one lockfile at the repository root.

| Path | What it is |
| --- | --- |
| `web/shell/` | `@rookery/shell`, the app: Vite, `@solidjs/vite-plugin`, SolidJS 2, happy-dom tests |
| `web/shell/src/shell.tsx` | The shell's state: the client, sign-in, the spaces, the connection, notifications and the plugins' halves |
| `web/shell/src/chrome/` | The chrome: the sidebar and its levels, the account menu, and the status bar, which shows only while the link to the server is down or plugins failed to load |
| `web/shell/src/pages/` | The shell's own pages, one folder per area, joined by `index.ts` |
| `web/shell/src/parts/` | What pages share: the view frame, live queries, actions, dialogs, row menus |
| `web/shell/src/plugin/` | The client-half host: a plugin's page and its tabs on a host's page |
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

## How the shell is laid out

The shell is designed from the person's objects outward. The sidebar is a stack
of levels: the top, then the containers a person drills into.

* The top holds Home and the Inbox, the Fleet, one row per space, and the
  plugins' own views.
* The Fleet holds hosts, terminals, jobs, schedules and roll-outs, then
  one row per host. A host holds its own places.
* A space holds its overview, the views its kind's plugins add, its documents and
  records, and its settings. Every call from a page under `/spaces/{id}` is scoped
  to that space.
* Administration and Settings open from the account menu.

A drill row (›) moves the sidebar alone and leaves the page as it is. Back goes
up a level. The level's switcher moves to a sibling, such as another host, and
keeps the place. The sidebar follows the page until the person drills by hand,
and that holds until the next page.

The pages keep to the same rules:

* No prose. A field's explanation is its `info` tip.
* Nothing for the normal state, and no zero counts.
* One empty state per view: a title and its action.
* A thing is where the person thinks of it, not where the server keeps it: a
  machine a provisioner makes is a host, so it is listed with the hosts and
  made from "Add host", and the provisioners are a tab of the hosts page.
* Adding is a header action with a plus. Deleting is in a row's menu, never a
  button on the row.
* Refresh is in the overflow menu.
* What the person may not change is not shown.

## Pages: the seam

A page area is a folder under `web/shell/src/pages/` whose `index.ts` exports an
`Area` (`pages/types.ts`):

```ts
// web/shell/src/pages/notes/index.ts
import { defineRoutes } from "@solidjs/router";

import { prefixMatch } from "../../chrome/levels.ts";
import type { Area } from "../types.ts";
import { NotesPage } from "./notes.tsx";
import { NotesSidebar, useNotesRow } from "./sidebar.tsx";

export const routes = defineRoutes([{ path: "/notes", component: NotesPage }]);
export const levels: Area["levels"] = [{ id: "notes", depth: 1, match: prefixMatch("/notes"), Sidebar: NotesSidebar }];
export const top: Area["top"] = [{ order: 30, use: useNotesRow }];
```

* `routes` is what the area answers, as `@solidjs/router` route definitions.
* `levels` are the containers in it. `match` says which addresses are inside
  one and with which values, such as a host's name. The deepest match wins.
  `Sidebar` draws the level with `LevelSidebar` from `chrome/sidebar.tsx`,
  which takes a back target, a title, a switcher from `useSiblings` and groups
  of entries. An entry with `drill` opens a level below.
* `top` adds groups to the top level, ordered by `order`. `use` runs in the
  sidebar's scope and returns the groups, so it may read live data.
* `overlays` are drawn over every page, such as the dialog the Spaces group's
  plus opens.

`web/shell/src/pages/index.ts` lists the areas (`AREAS`) and joins them. Adding
an area is one import and one entry there. No other file changes, and the
chrome never names an area.

Pages read the shell with `useShell()` from `src/shell.tsx`: `rookery` is the
typed client (`@rookery/client`), `signIn` who is signed in, `spaces` the space
list and the space the address is in, `connection` the events stream,
`notifications`, and `plugins` the installed client halves. A page never builds
a transport.

A page that shows data asks for it with `createLive` from `parts/data.ts`, which
asks again when one of the named events arrives:

```tsx
const hosts = createLive(() => shell.rookery.hosts.list({ limit: 200 }), { events: ["rookery/hosts."] });
```

A page in a space is drawn afresh for each space, so it reads the space once.

## Plugin hosting

A plugin's views are routes resolved by `src/plugin/route.tsx` from
`PluginService.Contributions`. A view that names a kind of space is a place in
each space of that kind, under `/spaces/{id}{route}`. An admin page is a place in
Administration, under `/admin/p/{plugin}/{page}`. A page is mounted with
`defineClient`'s contract, `mount(context, element)`, through
`src/plugin/mount.tsx`. That file also hands a host's page its tabs, and maps a
plugin's navigation into the space or Administration it is in.

A plugin's client half is loaded from `/plugins/{id}/{hash}/index.js`. A half
that fails to load is left out.

## The server's pages

`pages/admin/` is Administration: people, groups and access; plugins and
registries, spaces, the key ring and known hosts, the tailnet and the server's
status; secrets and connections; the datastore and documents across every space.

Three calls the pages needed are new in the contract, because nothing listed what
a page could browse:

* `DataService.ListCollections` lists the collections the caller holds view on
  in the space it is in, with their fields and whether they keep history. The
  datastore page's table is that list.
* `DocumentService.List` lists the documents the caller may read, newest first.
  The documents page has no editor: a document is edited by the CRDT client that
  syncs it, and the page shows what it is, where it hangs and its versions.
* `PluginService.InstallPackage` takes a whole package in one call, because a
  browser cannot stream a request body; the page's file field uses it, and a
  registry reference uses `InstallFromRegistry`.

Settings shows what the person may set for themselves and, when they may, for
everyone. The scope is a dropdown that shows only when there is more than one.
Only rows the person may change at the scope are shown. A space's own values are
in the space's Settings.

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
commit in `web/shell/package.json`. A part the shell needs and Nocturne lacks is
a change to Nocturne first, then a new pin.

# Working on Rookery's docs site

This folder is Rookery's documentation site, a static Fumadocs site on Next.js. It is published to GitHub Pages at https://rookery.helight.dev/. This file is for coding agents working in it.

## The docs base

The site follows the [docs base](https://github.com/aiviarium/docs-base) of the Aiviarium projects at commit `dcffeac`. The theme, layouts, landing page, header, badges and checks are the base's. Rookery's values are in `docs.config.ts` and its brand files in `public/brand/`.

To take a later version of the base, compare the base from `dcffeac` to its new commit and apply that change here. Rookery differs from the base in these places, which a merge has to keep:

| File | What Rookery adds |
| --- | --- |
| `package.json` | The name, and `scripts/gather.mjs` before `dev`, `build`, `typecheck` and after install. `npm run vale` runs Vale with the repository's `.vale.ini` |
| `src/app/docs/[[...slug]]/page.tsx` | No "Edit on GitHub" for the generated API pages |
| `scripts/check-links.ts` | Folders in parentheses, such as `(guide)`, have no part in the URL, and files in `public/` count as pages |
| `.gitignore` | The generated API pages, and `/screens/` for the screenshots of the site that are never committed |

Change the theme in the base, not here.

## Layout

| Path | What it is |
| --- | --- |
| `docs.config.ts` | Rookery's name, colour, links, badges, install command and landing cards |
| `public/brand/` | Rookery's icons and wordmarks, from the brand folder of aiviarium/.github |
| `content/docs/(guide)/` | The guide, in MDX, with `meta.json` for the order |
| `content/docs/api/` | The API overview, portal and testing pages. `api/host/` and `api/serve/` are generated and not committed |
| `openapi/` | The committed API documents, written by the Rust tests |
| `scripts/gather.mjs`, `scripts/reference.mjs` | Write the API pages from `openapi/` and copy the documents to `public/openapi/` |
| `scripts/site-screens.py` | Screenshots of the built site for review, into `screens/`, which git ignores |
| `scripts/check-links.ts` | The link check |
| `src/` | The base's routes, components, theme and helpers |

## Build and check

```bash
npm install         # also writes the API pages
npm run dev         # http://localhost:3000
npm run typecheck
npm run lint
npm run links
npm run vale        # must print nothing
npm run build       # writes out/; DOCS_BASE_PATH=/rookery as on GitHub Pages
```

The Docs workflow runs the type check, lint, link check and build. CI's lint job runs Vale on the content. Never commit screenshots of the site.

## Conventions

- Next.js 16 and Fumadocs 16 differ from older versions. Read `node_modules/next/dist/docs/` and the types in `node_modules/fumadocs-*` before changing routes or layouts.
- The site is a static export. Route handlers export `dynamic = 'force-static'`. Plain URLs to files in `public/` go through `asset()` so they get the base path.
- Keep project values in `docs.config.ts`. Code reads them from there.
- Follow the design rules of the base (`docs/design.md` there). The project colour never fills a panel, card, button or callout. Only the value of a badge is filled with it.
- Do not edit the pages under `content/docs/api/host` and `api/serve`. They are written again on every build.

<!-- vale off -->
<!-- BEGIN:nextjs-agent-rules -->

## This is NOT the Next.js you know

This version has breaking changes — APIs, conventions, and file structure may all differ from your training data. Read the relevant guide in `node_modules/next/dist/docs/` (resolved from this file's directory; in monorepos the `next` package may not be visible from the repo root) before writing any code. Heed deprecation notices.

This block is written and re-added by `next dev` — verify at `node_modules/next/dist/server/lib/generate-agent-files.js`. Removing it from a diff only re-creates the uncommitted change; committing it with your work keeps the tree clean.

<!-- END:nextjs-agent-rules -->
<!-- vale on -->

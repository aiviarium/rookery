// The Solid JSX transform for the tests.
//
// Solid 2 has no JSX runtime: its compiler turns JSX into DOM work, and Bun's
// own transform does not know it. This loads Solid's compiler for every .tsx a
// test imports, and serves the browser build of Solid's runtime where Bun would
// resolve the server one, so the tests have one DOM-capable copy.
import { readFileSync } from "node:fs";
import { transform } from "@solidjs/compiler";
import { plugin } from "bun";

// Bun resolves Solid's packages under the "node" condition, whose build has no
// DOM. Serve the browser build's contents instead.
const SERVER_TO_BROWSER: Array<[RegExp, string]> = [
  [/[\\/]solid-js[\\/]dist[\\/]server\.js$/, "solid.js"],
  [/@solidjs[\\/]web[\\/]dist[\\/]server\.js$/, "web.js"],
  [/[\\/]solid-js[\\/]dist[\\/]server\.dev\.js$/, "solid.dev.js"],
  [/@solidjs[\\/]web[\\/]dist[\\/]server\.dev\.js$/, "web.dev.js"],
];

// One copy of Solid for every importer. A Nocturne checkout linked in for local
// work brings its own node_modules, and two runtimes do not share an owner, so a
// Show inside one of its panels would never update. Vite dedupes these; here a
// copy outside the shell re-exports the shell's own.
const SHELL = new URL("..", import.meta.url).pathname;
/** home is where the shell's copy of a package resolves from: signals is Solid's own dependency. */
const SOLID = Bun.resolveSync("solid-js", SHELL);
const home = (name: string) => (name === "@solidjs/signals" ? SOLID : SHELL);
const OWN = ["solid-js", "@solidjs/web", "@solidjs/signals"].map((name) => {
  const file = Bun.resolveSync(name, home(name));
  return file.slice(0, file.indexOf(`/node_modules/${name}/`) + `/node_modules/${name}/`.length);
});
const PACKAGE = /[\\/]node_modules[\\/](solid-js|@solidjs[\\/]web|@solidjs[\\/]signals)[\\/]dist[\\/]/;

plugin({
  name: "solid",
  setup(build) {
    build.onLoad({ filter: /(solid-js|@solidjs[\\/]web|@solidjs[\\/]signals)[\\/]dist[\\/].*\.js$/ }, (args) => {
      const other = PACKAGE.exec(args.path);
      if (other !== null && !OWN.some((root) => args.path.startsWith(root))) {
        const name = other[1]!.replace("\\", "/");
        return { contents: `export * from ${JSON.stringify(Bun.resolveSync(name, home(name)))};`, loader: "js" };
      }
      const hit = SERVER_TO_BROWSER.find(([pattern]) => pattern.test(args.path));
      if (hit === undefined) return { contents: readFileSync(args.path, "utf8"), loader: "js" };
      return { contents: readFileSync(args.path.replace(/server(\.dev)?\.js$/, hit[1] ?? "solid.js"), "utf8"), loader: "js" };
    });

    build.onLoad({ filter: /\.tsx$/ }, (args) => {
      const result = transform(readFileSync(args.path, "utf8"), {
        filename: args.path,
        generate: "dom",
        hydratable: false,
      });
      return { contents: result.code, loader: "ts" };
    });
  },
});

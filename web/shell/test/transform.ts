// The Solid JSX transform for the tests.
//
// Solid 2 has no JSX runtime: its compiler turns JSX into DOM work, and Bun's
// own transform does not know it. This loads the compiler for every .tsx a test
// imports, and serves the browser build of Solid's runtime where Bun would
// resolve the server one, so the tests have one DOM-capable copy.
import { readFileSync } from "node:fs";
import { transformAsync } from "@babel/core";
import typescript from "@babel/preset-typescript";
import solid from "@solidjs/babel-plugin";
import { plugin } from "bun";

// Bun resolves Solid's packages under the "node" condition, whose build has no
// DOM. Serve the browser build's contents instead.
const SERVER_TO_BROWSER: Array<[RegExp, string]> = [
  [/[\\/]solid-js[\\/]dist[\\/]server\.js$/, "solid.js"],
  [/@solidjs[\\/]web[\\/]dist[\\/]server\.js$/, "web.js"],
  [/[\\/]solid-js[\\/]dist[\\/]server\.dev\.js$/, "solid.dev.js"],
  [/@solidjs[\\/]web[\\/]dist[\\/]server\.dev\.js$/, "web.dev.js"],
];

plugin({
  name: "solid",
  setup(build) {
    build.onLoad({ filter: /solid-js[\\/]dist[\\/]server(\.dev)?\.js$|web[\\/]dist[\\/]server(\.dev)?\.js$/ }, (args) => {
      const hit = SERVER_TO_BROWSER.find(([pattern]) => pattern.test(args.path));
      if (hit === undefined) return undefined;
      return { contents: readFileSync(args.path.replace(/server(\.dev)?\.js$/, hit[1]), "utf8"), loader: "js" };
    });

    build.onLoad({ filter: /\.tsx$/ }, async (args) => {
      const source = readFileSync(args.path, "utf8");
      const result = await transformAsync(source, {
        filename: args.path,
        plugins: [[solid, { generate: "dom", hydratable: false }]],
        presets: [typescript],
        babelrc: false,
        configFile: false,
      });
      return { contents: result?.code ?? "", loader: "js" };
    });
  },
});

// Solid JSX transform for .tsx and browser build of solid-js under Bun's test runner.

import { readFileSync } from "node:fs"
import { transformAsync } from "@babel/core"
import typescript from "@babel/preset-typescript"
import solid from "babel-preset-solid"
import { plugin } from "bun"

const SERVER_TO_BROWSER: Array<[RegExp, string]> = [
  [/solid-js[\\/]dist[\\/]server\.js$/, "solid.js"],
  [/solid-js[\\/]web[\\/]dist[\\/]server\.js$/, "web.js"],
  [/solid-js[\\/]store[\\/]dist[\\/]server\.js$/, "store.js"],
]

plugin({
  name: "solid",
  setup(build) {
    build.onLoad({ filter: /solid-js[\\/].*server\.js$|solid-js[\\/]dist[\\/]server\.js$/ }, (args) => {
      const hit = SERVER_TO_BROWSER.find(([re]) => re.test(args.path))
      if (!hit) return undefined
      return { contents: readFileSync(args.path.replace(/server\.js$/, hit[1]), "utf8"), loader: "js" }
    })
    build.onLoad({ filter: /\.tsx$/ }, async (args) => {
      const code = readFileSync(args.path, "utf8")
      const result = await transformAsync(code, {
        filename: args.path,
        presets: [[solid, { generate: "dom", hydratable: false }], typescript],
        babelrc: false,
        configFile: false,
      })
      return { contents: result?.code ?? "", loader: "js" }
    })
  },
})

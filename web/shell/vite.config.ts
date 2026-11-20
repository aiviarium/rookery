import solid from "@solidjs/vite-plugin";
import { defineConfig } from "vite";

// The build lands in the Go package that embeds it, so `go build` ships the
// shell. Vite names each file after its content, and the edge serves assets
// under that hash for a year.
export default defineConfig({
  plugins: [solid()],
  build: {
    outDir: "../../internal/webshell/dist",
    emptyOutDir: true,
    assetsDir: "assets",
    rolldownOptions: {
      // Nocturne is published as source and its index re-exports every part, including the rich text editor and
      // its ProseMirror chain. Its modules have no side effects (only its styles are imported for theirs), so the
      // parts a page does not use leave the bundle instead of shipping on every page.
      treeshake: {
        moduleSideEffects: (id: string) =>
          !(id.includes("/@aiviarium/nocturne/src/") && !id.endsWith(".css")),
      },
    },
  },
});

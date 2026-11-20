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
  },
});

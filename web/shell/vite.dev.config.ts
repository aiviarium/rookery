// Development only: serve the shell with hot reloading and proxy the edge's routes to ROOKERY_DEV_SERVER.
import { defineConfig, mergeConfig } from "vite";
import base from "./vite.config.ts";

const target = process.env.ROOKERY_DEV_SERVER ?? "http://127.0.0.1:4791";
const edge = "^/(rookery\\.|auth/|blobs/|ws/|plugins/|hooks/|healthz|hosts/[^/]+/ports/[^/]+)";

export default mergeConfig(
  base,
  defineConfig({
    server: { host: "127.0.0.1", port: 4793, strictPort: true, proxy: { [edge]: { target, ws: true, changeOrigin: false } } },
  }),
);

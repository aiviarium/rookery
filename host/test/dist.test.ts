// dist/ is the bundle of the extension host that a release of Rookery carries, so that a server runs
// it with Bun alone. It has to be what src/ builds to: `cd host && bun run build` writes it again.
import { expect, test } from "bun:test"
import { mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"

const host = fileURLToPath(new URL("..", import.meta.url))

test("dist is the bundle of src", async () => {
  const out = mkdtempSync(join(tmpdir(), "rookery-host-dist-"))
  try {
    const built = Bun.spawnSync(
      [
        process.execPath,
        "build",
        "src/main.ts",
        "src/worker.ts",
        "src/transform.ts",
        "--target=bun",
        "--minify",
        "--splitting",
        `--outdir=${out}`,
      ],
      { cwd: host },
    )
    expect(built.exitCode).toBe(0)
    const files = (dir: string) => readdirSync(dir).sort()
    expect(files(join(host, "dist"))).toEqual(files(out))
    for (const name of files(out)) {
      expect(
        readFileSync(join(host, "dist", name), "utf8"),
        `dist/${name} is stale: cd host && bun run build`,
      ).toBe(readFileSync(join(out, name), "utf8"))
    }
  } finally {
    rmSync(out, { recursive: true, force: true })
  }
})

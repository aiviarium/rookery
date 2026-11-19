// Makes this process non-dumpable (prctl PR_SET_DUMPABLE 0) before anything else runs: its
// /proc/PID/fd, environ and mem become root's, so the hosts of other plugins, which run as the same
// user, cannot open its pipes to the server or read its memory. A host that cannot do it does not run.
// It is a mitigation, not a sandbox: this process still reads whatever the server's user can.
import { dlopen, FFIType } from "bun:ffi"

const PR_SET_DUMPABLE = 4

/** The C library's names: glibc's, then musl's on x86-64 and on ARM. */
const LIBC = ["libc.so.6", "libc.musl-x86_64.so.1", "libc.musl-aarch64.so.1"]

function undumpable(): string | null {
  const tried: string[] = []
  for (const name of LIBC) {
    let libc: ReturnType<typeof open>
    try {
      libc = open(name)
    } catch (e) {
      tried.push(`${name}: ${String(e)}`)
      continue
    }
    try {
      const done = libc.symbols.prctl(PR_SET_DUMPABLE, 0, 0, 0, 0)
      return done === 0 ? null : `prctl answered ${done}`
    } finally {
      libc.close()
    }
  }
  return `no C library to call prctl in (${tried.join("; ")})`
}

function open(name: string) {
  return dlopen(name, {
    prctl: {
      args: [FFIType.i32, FFIType.u64, FFIType.u64, FFIType.u64, FFIType.u64],
      returns: FFIType.i32,
    },
  })
}

const failed = undumpable()
if (failed) {
  process.stderr.write(`[host] cannot make this process non-dumpable, so it does not run: ${failed}\n`)
  process.exit(1)
}

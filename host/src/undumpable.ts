// Makes this process non-dumpable (prctl PR_SET_DUMPABLE 0) before anything else runs: its
// /proc/PID/fd, environ and mem become root's, so the hosts of other plugins, which run as the same
// user, cannot open its pipes to the server or read its memory. A host that cannot do it does not run.
// It is a mitigation, not a sandbox: this process still reads whatever the server's user can.
import { dlopen, FFIType } from "bun:ffi"

const PR_SET_DUMPABLE = 4

function undumpable(): string | null {
  try {
    const libc = dlopen("libc.so.6", {
      prctl: {
        args: [FFIType.i32, FFIType.u64, FFIType.u64, FFIType.u64, FFIType.u64],
        returns: FFIType.i32,
      },
    })
    const done = libc.symbols.prctl(PR_SET_DUMPABLE, 0, 0, 0, 0)
    libc.close()
    return done === 0 ? null : `prctl answered ${done}`
  } catch (e) {
    return String(e)
  }
}

const failed = undumpable()
if (failed) {
  process.stderr.write(`[host] cannot make this process non-dumpable, so it does not run: ${failed}\n`)
  process.exit(1)
}

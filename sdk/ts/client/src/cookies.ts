// The cookies the edge sets, and the header it checks.
//
// A browser's calls are authenticated by the session cookie, which is HttpOnly,
// and the session's CSRF token travels in a cookie a script may read and in the
// `Rookery-CSRF` header of every call. The current space is the `Rookery-Space`
// header.

/** The session cookie's name. It is HttpOnly, so only the browser sends it. */
export const SESSION_COOKIE = "rookery_session";

/** The CSRF cookie's name, which the edge also gives the session. */
export const CSRF_COOKIE = "rookery_csrf";

/** The header that echoes the CSRF token on every call. */
export const CSRF_HEADER = "Rookery-CSRF";

/** The header that names the caller's current space. */
export const SPACE_HEADER = "Rookery-Space";

/** readCookie returns a cookie's value, or undefined when it is not set. */
export function readCookie(name: string): string | undefined {
  if (typeof document === "undefined" || typeof document.cookie !== "string") {
    return undefined;
  }
  for (const pair of document.cookie.split(";")) {
    const [key, ...rest] = pair.split("=");
    if (key === undefined || rest.length === 0) continue;
    if (key.trim() !== name) continue;
    return decodeURIComponent(rest.join("=").trim());
  }
  return undefined;
}

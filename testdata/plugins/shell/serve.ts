// serve.ts serves the fixture shell by hand, for trying it in a browser without
// a Rookery server: `bun run serve.ts`, then open the address it prints.
//
// The page it serves is the one Rookery serves for `/` when the shell plugin is
// installed. Its calls go to the server the page's origin names, so a shell tried
// this way needs `createRookery({ baseUrl })` or the server on another port.

const root = new URL("./", import.meta.url).pathname;
const port = Number(process.env["PORT"] ?? 5173);

const page = await Bun.file(new URL("./index.html", import.meta.url)).text();
const script = Bun.file(new URL("./index.js", import.meta.url));

const server = Bun.serve({
  port,
  async fetch(request) {
    const { pathname } = new URL(request.url);
    if (pathname === "/" || pathname === "/index.html") {
      return new Response(page, { headers: { "content-type": "text/html; charset=utf-8" } });
    }
    if (pathname === "/index.js") {
      return new Response(script, { headers: { "content-type": "text/javascript" } });
    }
    return new Response("Not found", { status: 404 });
  },
});

console.log(`the fixture shell is at http://localhost:${server.port}/, from ${root}`);

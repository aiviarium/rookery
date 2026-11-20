// The fixture shell's page: sign in, list what the installed plugins add, and
// mount a client half's view in the outlet.
//
// It is the whole shell contract in one file. Rookery serves this page for `/`
// and does not ship a UI of its own: a real shell is a plugin like this one.

import {
  clientContext,
  createRookery,
  loadClientHalves,
  type LoadedClientHalf,
} from "@rookery/client";

const rookery = createRookery();

const who = document.querySelector("#who");
const nav = document.querySelector("#nav");
const outlet = document.querySelector("#outlet");

const loaded = await loadClientHalves(rookery, {
  onError(error, entry) {
    console.warn(`the client half of ${entry.plugin} did not load: ${error.message}`);
  },
});

const whoIsIt = rookery.signIn.get();
const name = whoIsIt.status === "signed-in" ? whoIsIt.user.name : "Not signed in";
who?.replaceChildren(document.createTextNode(name));

const results: LoadedClientHalf[] = [...loaded];
for (const half of results) {
  for (const view of half.contributes.views ?? []) {
    const link = document.createElement("a");
    link.href = view.route;
    link.textContent = view.label;
    nav?.append(link);
  }
}

// The first view any client half declares is the one the fixture mounts.
for (const half of results) {
  const view = half.contributes.views?.[0];
  const mount = view === undefined ? undefined : half.half.views?.[view.id];
  if (view === undefined || mount === undefined || !(outlet instanceof HTMLElement)) {
    continue;
  }
  mount(clientContext(rookery, half, (path) => history.pushState({}, "", path)), outlet);
  break;
}

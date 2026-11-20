import "@aiviarium/nocturne/styles/index.css";
import "./app.css";

import { render } from "@solidjs/web";

import { App } from "./app.tsx";

const root = document.getElementById("shell");
if (root === null) throw new Error("the page has no #shell element");

render(() => <App />, root);

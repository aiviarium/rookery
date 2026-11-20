// The shell's tests run against a page: happy-dom gives Solid an element tree,
// and the tests replace the client with one that answers from a fake transport.
import { GlobalRegistrator } from "@happy-dom/global-registrator";
import "./transform.ts";

GlobalRegistrator.register({ url: "https://rookery.test/" });

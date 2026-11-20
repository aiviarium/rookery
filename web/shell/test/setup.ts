// The shell's tests run against a page: happy-dom gives Solid an element tree,
// and the tests replace the client with one that answers from a fake transport.
import { GlobalRegistrator } from "@happy-dom/global-registrator";
import "./transform.ts";

GlobalRegistrator.register({ url: "https://rookery.test/" });

// happy-dom drops a falsy `textContent`, so `element.textContent = 0` leaves the
// element empty and the next write to the text node that Solid expects throws.
// A browser sets "0". Coerce the value the way a browser does before handing it
// to happy-dom.
const textContent = Object.getOwnPropertyDescriptor(Element.prototype, "textContent");
if (textContent?.set !== undefined) {
  Object.defineProperty(Element.prototype, "textContent", {
    configurable: true,
    get: textContent.get,
    set(value: unknown) {
      textContent.set?.call(this, value === null || value === undefined ? "" : String(value));
    },
  });
}

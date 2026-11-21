// The shell's tests run against a page: happy-dom gives Solid an element tree,
// and the tests replace the client with one that answers from a fake transport.
import { GlobalRegistrator } from "@happy-dom/global-registrator";
import "./transform.ts";

GlobalRegistrator.register({ url: "https://rookery.test/", width: 1280, height: 800 });

// What ui-sdk's testing guide says a DOM without layout needs, as Nocturne's own
// tests set it up. happy-dom has no popover API, so this stands in for the top
// layer, and an animation here is one that is already done.
const proto = HTMLElement.prototype as HTMLElement & { _open?: boolean };
if (typeof proto.showPopover !== "function") {
  Object.assign(proto, {
    showPopover(this: HTMLElement & { _open?: boolean }) {
      this._open = true;
      this.dispatchEvent(new Event("toggle"));
    },
    hidePopover(this: HTMLElement & { _open?: boolean }) {
      this._open = false;
      this.dispatchEvent(new Event("toggle"));
    },
    togglePopover(this: HTMLElement & { _open?: boolean; showPopover(): void; hidePopover(): void }, force?: boolean) {
      const next = force ?? !this._open;
      if (next) this.showPopover();
      else this.hidePopover();
      return next;
    },
  });
}
Element.prototype.getAnimations = () => [];
Element.prototype.scrollIntoView ??= () => {};
Element.prototype.checkVisibility = () => true;
document.elementFromPoint = () => null;
document.elementsFromPoint = () => [];
Element.prototype.animate = () => {
  const done = Promise.resolve();
  return {
    finished: done,
    ready: done,
    cancel() {},
    finish() {},
    play() {},
    pause() {},
    reverse() {},
    commitStyles() {},
    persist() {},
    addEventListener() {},
    removeEventListener() {},
    playState: "finished",
    currentTime: null,
    effect: null,
    onfinish: null,
    oncancel: null,
  } as unknown as Animation;
};

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

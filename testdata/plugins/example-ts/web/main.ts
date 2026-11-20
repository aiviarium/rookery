// The client half of the TypeScript fixture: the Notes view over its collection.
//
// The shell loads this from `/plugins/example-ts/<hash>/index.js`, gives the view
// the context `@rookery/client` describes, and the view draws into the element
// it is given. Records arrive as CBOR, which `cbor` reads.

import { cbor } from "@rookery/client";
import { defineClient } from "@rookery/sdk/client";

/** Note is a record of the fixture's collection. */
interface Note {
  readonly title: string;
  readonly body?: string;
}

export default defineClient({
  views: {
    /** notes draws the notes of the space the shell is in. */
    notes(context, element) {
      const controller = new AbortController();
      element.textContent = "Loading the notes…";

      const load = async () => {
        const found = await context.rookery.data.find(
          { collection: `${context.plugin.id}/notes` },
          { signal: controller.signal },
        );
        const notes = found.recordsCbor
          .map((record) => cbor.decode<Note>(record))
          .filter((note): note is Note => note !== undefined);

        if (notes.length === 0) {
          element.textContent = "No notes yet.";
          return;
        }
        const list = document.createElement("ul");
        for (const note of notes) {
          const item = document.createElement("li");
          item.textContent = note.title;
          list.append(item);
        }
        element.replaceChildren(list);
      };

      load().catch((error: unknown) => {
        if (!controller.signal.aborted) {
          element.textContent = `The notes did not load: ${String(error)}`;
        }
      });

      return () => controller.abort();
    },
  },
});

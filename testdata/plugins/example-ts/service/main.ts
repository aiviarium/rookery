// The service of the TypeScript fixture.
//
// It serves what its manifest declares: NoteService/Summarize, and the reindex
// job. The procedure's arguments and its answer are CBOR, as every dynamic value
// on Rookery's wire is, and the job's arguments are too.
//
// The server starts this with `bun run service/main.ts`, naming the two sockets
// and the token in its environment.

import { cbor, servePlugin } from "@rookery/sdk";

/** Note is a record of the fixture's collection. */
export interface Note {
  readonly title: string;
  readonly body?: string;
}

/** summarise is NoteService/Summarize: the notes in a sentence. */
export function summarise(notes: readonly Note[]): string {
  if (notes.length === 0) {
    return "No notes yet.";
  }
  const titles = notes.map((note) => note.title).join(", ");
  return `${notes.length} note${notes.length === 1 ? "" : "s"}: ${titles}`;
}

/** reindex works the reindex job: it reads a Note from its arguments. */
export function reindex(args: Uint8Array): string {
  const note = cbor.decode<Note>(args);
  return note === undefined ? "reindexed every note" : `reindexed ${note.title}`;
}

const plugin = await servePlugin(() => {
  // The procedure and the job kind are registered here once the plugin service
  // proto is on `go`; until then the socket is what the fixture proves.
});

const stop = (): void => {
  void plugin.close().then(() => process.exit(0));
};
process.on("SIGTERM", stop);
process.on("SIGINT", stop);

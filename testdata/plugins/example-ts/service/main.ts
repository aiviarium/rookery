import { Code, ConnectError } from "@connectrpc/connect";
import { AuthService } from "@rookery/proto";
import { CallerTokenHeader, cbor, PluginSocketClient, servePlugin } from "@rookery/sdk";

import { NoteService } from "../../../../sdk/ts/proto/src/gen/example/v1/note_pb.js";

const client = new PluginSocketClient();
const lifetime = new AbortController();
let worker: Promise<void> | undefined;

const plugin = await servePlugin((router) => {
  router.service(NoteService, {
    async summarize(request, context) {
      const caller = client.asCaller(context.requestHeader.get(CallerTokenHeader));
      const me = await caller.core(AuthService).me({}, { signal: context.signal });
      const summary = `${request.titles.length} note(s): ${request.titles.join(", ")}`;
      return { summary, user: me.user?.email ?? "" };
    },
  });
}, {
  lifecycle: {
    init() {
      worker ??= client.runWorker(["reindex"], async (work) => {
        await work.line("reindexing notes");
        const note = cbor.decode<{ title: string }>(work.job.argsCbor);
        return note ? `reindexed ${note.title}` : "reindexed";
      }, lifetime.signal).catch((error: unknown) => {
        if (ConnectError.from(error).code !== Code.Canceled) {
          console.error("The notes worker ended.");
          process.exitCode = 1;
          void plugin.close();
        }
      });
      return {};
    },
  },
});

function stop(): void {
  lifetime.abort();
  void plugin.close().then(() => worker).then(() => process.exit(process.exitCode ?? 0));
}
process.on("SIGTERM", stop);
process.on("SIGINT", stop);

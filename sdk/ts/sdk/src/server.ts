import type { DescService } from "@bufbuild/protobuf";
import { timestampDate } from "@bufbuild/protobuf/wkt";
import { Code, ConnectError, createClient, type Client, type Interceptor, type Transport } from "@connectrpc/connect";
import { createGrpcTransport } from "@connectrpc/connect-node";
import { cbor } from "@rookery/client";
import { ServerService, type Job, type ServerServiceReportWorkRequest } from "@rookery/proto";
import { createConnection } from "node:net";

import { pluginConfig, type PluginConfig } from "./config.ts";

/** The short-lived user credential forwarded to a plugin procedure. */
export const CallerTokenHeader = "Rookery-Caller-Token";

/** PluginSocketClient exposes generated client calls on the deployment socket. */
export class PluginSocketClient {
  readonly server: Client<typeof ServerService>;

  constructor(
    private readonly config: PluginConfig = pluginConfig(),
    private readonly transport: Transport = socketTransport(config),
  ) {
    this.server = this.core(ServerService);
  }

  /** core connects a generated client to the same authenticated socket. */
  core<S extends DescService>(service: S): Client<S> {
    return createClient(service, this.transport);
  }

  /** asCaller delegates a forwarded user on subsequent socket requests. */
  asCaller(token: string | null): PluginSocketClient {
    if (!token) return this;
    const transport = this.transport;
    const delegated: Transport = {
      unary(service, signal, timeout, headers, input, context) {
        return transport.unary(service, signal, timeout, callerHeaders(headers, token), input, context);
      },
      stream(service, signal, timeout, headers, input, context) {
        return transport.stream(service, signal, timeout, callerHeaders(headers, token), input, context);
      },
    };
    return new PluginSocketClient(this.config, delegated);
  }

  /** runWorker claims and reports one job at a time until the signal aborts. */
  async runWorker(
    kinds: readonly string[],
    handle: (work: Work) => Promise<unknown>,
    signal: AbortSignal,
  ): Promise<void> {
    for await (const claimed of this.server.work({ kinds: [...kinds] }, { signal })) {
      if (!claimed.job || !claimed.deadline) throw new Error("Rookery returned an incomplete claim");
      const { job, claimToken } = claimed;
      const lifetime = new AbortController();
      const deadline = AbortSignal.timeout(Math.max(0, timestampDate(claimed.deadline).getTime() - Date.now()));
      const jobSignal = AbortSignal.any([signal, lifetime.signal, deadline]);
      const report = async (
        update: ServerServiceReportWorkRequest["report"],
        reportSignal = jobSignal,
      ): Promise<void> => {
        await this.server.reportWork({ jobId: job.id, claimToken, report: update }, { signal: reportSignal });
      };

      const heartbeat = keepClaimAlive(report, jobSignal).catch((error: unknown) => {
        lifetime.abort(error);
        return error;
      });
      let outcome: ServerServiceReportWorkRequest["report"];
      try {
        const result = await handle({
          job,
          signal: jobSignal,
          caller: this.asCaller(claimed.callerToken),
          line: (text) => report({ case: "line", value: text }),
          progress: (value) => report({ case: "progressCbor", value: cbor.encode(value ?? null) }),
        });
        outcome = { case: "resultCbor", value: cbor.encode(result ?? null) };
      } catch (error) {
        outcome = { case: "error", value: error instanceof Error ? error.message : String(error) };
      } finally {
        lifetime.abort();
      }
      const heartbeatFailure = await heartbeat;
      if (heartbeatFailure) {
        if (ConnectError.from(heartbeatFailure).code === Code.Aborted) continue;
        throw heartbeatFailure;
      }
      if (jobSignal.aborted && (signal.aborted || deadline.aborted)) continue;
      try {
        await report(outcome, signal);
      } catch (error) {
        if (ConnectError.from(error).code !== Code.Aborted) throw error;
      }
    }
  }
}

/** Work is one job attempt. Its signal ends when the claim can no longer run. */
export interface Work {
  readonly job: Job;
  readonly caller: PluginSocketClient;
  readonly signal: AbortSignal;
  line: (text: string) => Promise<void>;
  progress: (value: unknown) => Promise<void>;
}

function callerHeaders(headers: HeadersInit | undefined, token: string): Headers {
  const result = new Headers(headers);
  result.set(CallerTokenHeader, token);
  return result;
}

function socketTransport(config: PluginConfig): Transport {
  const authenticate: Interceptor = (next) => async (request) => {
    request.header.set("Authorization", `Bearer ${config.token}`);
    return next(request);
  };
  return createGrpcTransport({
    baseUrl: "http://rookery",
    nodeOptions: { createConnection: () => createConnection(config.socket) },
    interceptors: [authenticate],
  });
}

async function keepClaimAlive(
  report: (update: ServerServiceReportWorkRequest["report"]) => Promise<void>,
  signal: AbortSignal,
): Promise<void> {
  while (!signal.aborted) {
    await waitForHeartbeat(signal);
    if (!signal.aborted) {
      try {
        await report({ case: "heartbeat", value: true });
      } catch (error) {
        if (!signal.aborted) throw error;
      }
    }
  }
}

function waitForHeartbeat(signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    const done = (): void => {
      clearTimeout(timer);
      signal.removeEventListener("abort", done);
      resolve();
    };
    const timer = setTimeout(done, 5_000);
    signal.addEventListener("abort", done, { once: true });
  });
}

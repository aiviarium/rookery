// How the infrastructure pages read what the edge gives them: a host's state, a
// job's state, a roll-out's progress, and the times around them.
//
// The words are the CLI's, so a page and a terminal read the same.
import type { DotStatus } from "@aiviarium/nocturne";
import type { Host, HostType, Job, ProvisionerResource, Rollout } from "@rookery/proto";
import { cbor } from "@rookery/client";

/** Shown is a state as a page draws it: the words, and the dot Nocturne's `State` takes. */
export interface Shown {
  readonly label: string;
  readonly tone: DotStatus;
}

/** hostState is a host's apply progress. */
export function hostState(host: Host): Shown {
  if (host.appliedDigest === "") return { label: "Never applied", tone: "idle" };
  if (host.digest !== "" && host.appliedDigest !== host.digest) return { label: "Behind", tone: "warn" };
  return { label: "Current", tone: "ok" };
}

/** hostIssue is what is out of the ordinary about a host, or nothing while it runs its own profile. */
export function hostIssue(host: Host): Shown | undefined {
  const state = hostState(host);
  if (state.tone === "ok") return undefined;
  return state.tone === "idle" ? { label: "Not set up", tone: "idle" } : state;
}

/** hostTypeLabel is what a host's type is called. */
export function hostTypeLabel(type: HostType): string {
  switch (type) {
    case 1:
      return "SSH";
    case 2:
      return "Managed";
    case 3:
      return "Workstation";
    default:
      return "Unspecified";
  }
}

/** hostTypeName is a host type as a manifest names it. */
export function hostTypeName(type: HostType): string {
  switch (type) {
    case 3:
      return "workstation";
    case 2:
      return "managed";
    default:
      return "ssh";
  }
}

/** hostTypeTone is a host type's badge tone. */
export function hostTypeTone(type: HostType): "neutral" | "info" | "accent" {
  switch (type) {
    case 3:
      return "accent";
    case 2:
      return "info";
    default:
      return "neutral";
  }
}

/** jobState is a job's state. */
export function jobState(state: string): Shown {
  switch (state) {
    case "queued":
      return { label: "Queued", tone: "idle" };
    case "running":
      return { label: "Running", tone: "live" };
    case "succeeded":
      return { label: "Succeeded", tone: "ok" };
    case "failed":
      return { label: "Failed", tone: "error" };
    case "cancelled":
      return { label: "Cancelled", tone: "neutral" };
    case "interrupted":
      return { label: "Interrupted", tone: "warn" };
    default:
      return { label: state === "" ? "Unknown" : state, tone: "neutral" };
  }
}

/** jobActive says whether a job may still write output. */
export function jobActive(job: Job | undefined): boolean {
  return job?.state === "queued" || job?.state === "running";
}

/** rolloutState is a roll-out's state. */
export function rolloutState(state: string): Shown {
  switch (state) {
    case "running":
      return { label: "Running", tone: "live" };
    case "paused":
      return { label: "Paused", tone: "warn" };
    case "succeeded":
      return { label: "Succeeded", tone: "ok" };
    case "failed":
      return { label: "Failed", tone: "error" };
    case "cancelled":
      return { label: "Cancelled", tone: "neutral" };
    default:
      return { label: state === "" ? "Unknown" : state, tone: "neutral" };
  }
}

/** rolloutActive says whether a roll-out may still change. */
export function rolloutActive(rollout: Rollout | undefined): boolean {
  return rollout?.state === "running" || rollout?.state === "paused";
}

/** rolloutHostState is one host's place in a roll-out. */
export function rolloutHostState(state: string): Shown {
  switch (state) {
    case "pending":
      return { label: "Pending", tone: "idle" };
    case "running":
      return { label: "Running", tone: "live" };
    case "succeeded":
      return { label: "Succeeded", tone: "ok" };
    case "failed":
      return { label: "Failed", tone: "error" };
    case "skipped":
      return { label: "Skipped", tone: "neutral" };
    case "cancelled":
      return { label: "Cancelled", tone: "neutral" };
    default:
      return { label: state === "" ? "Unknown" : state, tone: "neutral" };
  }
}

/** Timestamp is the part of a protobuf timestamp a page formats. */
export interface Timestamp {
  readonly seconds: bigint;
  readonly nanos: number;
}

/** dateOf is a protobuf timestamp as a Date, or undefined when there is none. */
export function dateOf(at: Timestamp | undefined): Date | undefined {
  if (at === undefined) return undefined;
  return new Date(Number(at.seconds) * 1000 + Math.floor(at.nanos / 1e6));
}

/** relativeTime is a timestamp as "4 min ago", or "" when there is none. */
export function relativeTime(date: Date | undefined): string {
  if (date === undefined) return "";
  const seconds = Math.round((date.getTime() - Date.now()) / 1000);
  const ahead = seconds > 0;
  const value = Math.abs(seconds);
  if (value < 10) return ahead ? "soon" : "just now";
  const text =
    value < 60
      ? `${Math.round(value)} s`
      : value < 3600
        ? `${Math.round(value / 60)} min`
        : value < 86_400
          ? `${Math.round(value / 3600)} h`
          : `${Math.round(value / 86_400)} d`;
  return ahead ? `in ${text}` : `${text} ago`;
}

/** labelsText is a host's labels as "role=desk site=lab", sorted. */
export function labelsText(labels: Record<string, string>): string {
  return Object.keys(labels)
    .sort()
    .map((key) => `${key}=${labels[key]}`)
    .join(" ");
}

/** errorMessage is anything thrown, as one sentence. */
export function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}

/** bytesSize is a byte count as "1.2 MB". */
export function bytesSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}

/** machineAddress is a machine resource's address, or a word saying there is none. */
export function machineAddress(resource: ProvisionerResource): string {
  return resource.address === "" ? "Not known yet" : resource.address;
}

/** JOB_KINDS are what the core's job kinds are called; a plugin's kind is shown as it is. */
const JOB_KINDS: Record<string, string> = {
  "host.apply": "Apply",
  "host.action": "Action",
  "host.contact": "First contact",
  "host.create": "Create machine",
  "host.remove": "Remove machine",
};

/** jobKindLabel is a job's kind as a person reads it. */
export function jobKindLabel(kind: string): string {
  return JOB_KINDS[kind] ?? kind;
}

/**
 * jobTitle names a job as a person reads it: an action by the action it ran,
 * such as "Update packages", and every other job by its kind.
 */
export function jobTitle(job: Pick<Job, "kind" | "argsCbor">): string {
  if (job.kind === "host.action" && job.argsCbor.length > 0) {
    try {
      const name = cbor.decode<{ action?: string }>(job.argsCbor)?.action ?? "";
      if (name !== "") {
        const words = name.replace(/[-_.]+/g, " ").trim();
        return words.charAt(0).toUpperCase() + words.slice(1);
      }
    } catch {
      // A job whose arguments do not decode is named by its kind.
    }
  }
  return jobKindLabel(job.kind);
}

/** resourceLabel is a job's resource without its scheme, such as "forge" for host://forge. */
export function resourceLabel(resource: string): string {
  const at = resource.indexOf("://");
  return at < 0 ? resource : resource.slice(at + 3);
}

/** resourcePath is the shell's place for a resource, such as a host's page for host://forge, if it has one. */
export function resourcePath(resource: string): string | undefined {
  if (resource.startsWith("host://")) return `/hosts/${encodeURIComponent(resource.slice("host://".length))}`;
  if (resource.startsWith("rollout://")) return `/rollouts/${encodeURIComponent(resource.slice("rollout://".length))}`;
  return undefined;
}

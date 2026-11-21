// A host's profile: what an apply makes of it. The page edits the parts a person
// changes by hand — how Rookery reaches the host, which type it is, the desktop
// of a workstation, its registries and the plugin features it installs — and
// saves the profile as a whole, which is the digest. A saved change leaves the
// host behind until it is applied.
import { For, Show, createMemo, createSignal, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import { cbor } from "@rookery/client";
import { HostType, type Host } from "@rookery/proto";
import { Button, Callout, Card, IconButton, Select, Switch, TextArea, TextField } from "@aiviarium/nocturne";

import { createAction } from "../../../parts/action.ts";
import { View } from "../../../parts/view.tsx";
import { useShell } from "../../../shell.tsx";
import { jobPath } from "../jobs/rows.tsx";
import { hostCrumbs, useHost } from "./domain.tsx";

/** FeatureDraft is one feature row, with its variables as JSON. */
interface FeatureDraft {
  readonly plugin: string;
  readonly feature: string;
  readonly vars: string;
}

/** Draft is the form, as text the person types. */
interface Draft {
  readonly type: string;
  readonly address: string;
  readonly port: string;
  readonly sshUser: string;
  readonly operator: string;
  readonly tailnet: boolean;
  readonly registries: string;
  readonly desktopUser: string;
  readonly width: string;
  readonly height: string;
  readonly gpu: boolean;
  readonly features: readonly FeatureDraft[];
}

function draftOf(host: Host): Draft {
  const profile = host.profile;
  return {
    type: String(profile?.type ?? host.type ?? HostType.SSH),
    address: profile?.address ?? "",
    port: String(profile?.port || 22),
    sshUser: profile?.sshUser || "ops",
    operator: profile?.operator || "ops",
    tailnet: profile?.tailnet ?? true,
    registries: (profile?.registries ?? []).join(", "),
    desktopUser: profile?.desktop?.user || "dev",
    width: String(profile?.desktop?.width || 1920),
    height: String(profile?.desktop?.height || 1080),
    gpu: profile?.desktop?.gpu ?? false,
    features: (profile?.features ?? []).map((feature) => ({
      plugin: feature.plugin,
      feature: feature.feature,
      vars: feature.varsCbor.length === 0 ? "" : JSON.stringify(cbor.decode(feature.varsCbor) ?? {}, undefined, 2),
    })),
  };
}

/** problem is what keeps the draft from being saved, if anything. */
function problem(draft: Draft): string | undefined {
  if (draft.address.trim() === "") return "The profile needs the address Rookery reaches the host at.";
  const port = Number(draft.port);
  if (!Number.isInteger(port) || port <= 0 || port > 65535) return "The SSH port is 1 to 65535.";
  if (Number(draft.type) === HostType.WORKSTATION && (Number(draft.width) <= 0 || Number(draft.height) <= 0)) {
    return "The desktop's screen needs a width and a height.";
  }
  for (const feature of draft.features) {
    if (feature.plugin.trim() === "" || feature.feature.trim() === "") return "Each feature names its plugin and the feature.";
    if (feature.vars.trim() === "") continue;
    try {
      JSON.parse(feature.vars);
    } catch {
      return `The variables of ${feature.plugin}/${feature.feature} are not JSON.`;
    }
  }
  return undefined;
}

/** HostProfilePage is the profile place of a host. */
export function HostProfilePage(): JSX.Element {
  const domain = useHost();
  return (
    <Show
      when={domain.host() !== undefined}
      fallback={<View id="host-profile" crumbs={hostCrumbs(domain.name(), "Profile")} />}
    >
      <ProfileForm host={untrack(() => domain.host()!)} />
    </Show>
  );
}

function ProfileForm(props: { host: Host }): JSX.Element {
  const shell = useShell();
  const domain = useHost();
  // The form starts from the host as it was when the page opened. A change
  // someone else saves meanwhile does not overwrite what is being typed.
  const initial = untrack(() => draftOf(props.host));
  const [draft, setDraft] = createSignal<Draft>(initial);
  const [saved, setSaved] = createSignal<Draft>(initial);
  const [shown, setShown] = createSignal<string>();
  const dirty = createMemo(() => JSON.stringify(draft()) !== JSON.stringify(saved()));
  const set = (patch: Partial<Draft>) => {
    setShown(undefined);
    setDraft((previous) => ({ ...previous, ...patch }));
  };
  const setFeature = (index: number, patch: Partial<FeatureDraft>) =>
    set({ features: draft().features.map((feature, at) => (at === index ? { ...feature, ...patch } : feature)) });
  const workstation = () => Number(draft().type) === HostType.WORKSTATION;

  const save = createAction(async () => {
    const current = draft();
    await shell.rookery.hosts.setProfile({
      name: domain.name(),
      profile: {
        type: Number(current.type) as HostType,
        address: current.address.trim(),
        port: Number(current.port),
        sshUser: current.sshUser.trim(),
        operator: current.operator.trim(),
        tailnet: current.tailnet,
        registries: current.registries
          .split(",")
          .map((name) => name.trim())
          .filter((name) => name !== ""),
        features: current.features.map((feature) => ({
          plugin: feature.plugin.trim(),
          feature: feature.feature.trim(),
          varsCbor:
            feature.vars.trim() === "" ? new Uint8Array() : cbor.encode(JSON.parse(feature.vars) as Record<string, unknown>),
        })),
        desktop: workstation()
          ? { user: current.desktopUser.trim(), width: Number(current.width), height: Number(current.height), gpu: current.gpu }
          : undefined,
      },
    });
    setSaved(current);
    domain.refresh();
  }, { done: "Profile saved" });

  const apply = createAction(async () => {
    const started = await shell.rookery.hosts.apply({ name: domain.name() });
    shell.navigate(jobPath(started.job));
  }, { failed: "The apply did not start" });

  const submit = () => {
    const message = problem(draft());
    if (message !== undefined) {
      setShown(message);
      return;
    }
    void save.run();
  };

  const behind = () => {
    const host = domain.host();
    return !dirty() && host !== undefined && host.appliedDigest !== host.digest;
  };

  return (
    <View
      id="host-profile"
      crumbs={hostCrumbs(domain.name(), "Profile")}
      reading
      actions={[
        { id: "reset", label: "Discard changes", icon: "rotate-ccw", overflow: true, disabled: !dirty(), onClick: () => set(saved()) },
        {
          id: "save",
          label: save.busy() ? "Saving…" : "Save",
          icon: "check",
          primary: true,
          disabled: !dirty() || save.busy(),
          onClick: submit,
        },
      ]}
      error={shown() ?? save.error()}
    >
      <Show when={behind()}>
        <Callout
          tone="warning"
          action={
            <Button size="sm" loading={apply.busy()} onClick={() => void apply.run()}>
              Apply now
            </Button>
          }
        >
          Not applied yet: the host runs an older profile than this one.
        </Callout>
      </Show>
      <form
        class="nc-stack shell-form"
        onSubmit={(event) => {
          event.preventDefault();
          submit();
        }}
      >
        <Card title="Connection">
          <p class="nc-meta shell-lede">How Rookery reaches the host over SSH.</p>
          <div class="nc-stack">
            <Select
              label="Type"
              value={draft().type}
              options={[
                { value: String(HostType.SSH), label: "SSH", description: "A machine Rookery reaches and applies." },
                { value: String(HostType.MANAGED), label: "Managed", description: "Rookery runs its services and agents." },
                { value: String(HostType.WORKSTATION), label: "Workstation", description: "A managed host with a desktop." },
              ]}
              onChange={(type) => set({ type })}
            />
            <TextField
              label="Address"
              mono
              help="Changing the address of an existing host needs an admin."
              value={draft().address}
              onInput={(event) => set({ address: event.currentTarget.value })}
            />
            <div class="shell-form__row">
              <TextField label="SSH port" mono value={draft().port} onInput={(event) => set({ port: event.currentTarget.value })} />
              <TextField label="SSH user" mono value={draft().sshUser} onInput={(event) => set({ sshUser: event.currentTarget.value })} />
              <TextField label="Operator" mono value={draft().operator} onInput={(event) => set({ operator: event.currentTarget.value })} />
            </div>
            <Switch label="Join the tailnet" checked={draft().tailnet} onChange={(tailnet) => set({ tailnet })} />
          </div>
        </Card>
        <Show when={workstation()}>
          <Card title="Desktop">
          <p class="nc-meta shell-lede">The session noVNC shows on the Desktop place.</p>
            <div class="nc-stack">
              <div class="shell-form__row">
                <TextField label="User" mono value={draft().desktopUser} onInput={(event) => set({ desktopUser: event.currentTarget.value })} />
                <TextField label="Width" value={draft().width} onInput={(event) => set({ width: event.currentTarget.value })} />
                <TextField label="Height" value={draft().height} onInput={(event) => set({ height: event.currentTarget.value })} />
              </div>
              <Switch label="Give the desktop user the GPU" checked={draft().gpu} onChange={(gpu) => set({ gpu })} />
            </div>
          </Card>
        </Show>
        <Card title="Registries">
          <p class="nc-meta shell-lede">Connection names. The apply writes their logins on the host.</p>
          <TextField
            aria-label="Registries"
            mono
            placeholder="ghcr, docker-hub"
            help="Separate names with commas."
            value={draft().registries}
            onInput={(event) => set({ registries: event.currentTarget.value })}
          />
        </Card>
        <Card
          title="Features"
          action={
            <Button size="sm" icon="plus" onClick={() => set({ features: [...draft().features, { plugin: "", feature: "", vars: "" }] })}>
              Add feature
            </Button>
          }
        >
          <p class="nc-meta shell-lede">Plugin features the apply installs, in the order they run.</p>
          <Show when={draft().features.length > 0} fallback={<p class="nc-muted">No features. The host gets the core set up only.</p>}>
            <div class="nc-stack">
              <For each={draft().features}>
                {(feature, index) => (
                  <div class="shell-feature nc-stack" data-part="profile-feature">
                    <div class="shell-form__row shell-form__row--end">
                      <TextField
                        label="Plugin"
                        mono
                        placeholder="example"
                        value={feature.plugin}
                        onInput={(event) => setFeature(index(), { plugin: event.currentTarget.value })}
                      />
                      <TextField
                        label="Feature"
                        mono
                        placeholder="agent"
                        value={feature.feature}
                        onInput={(event) => setFeature(index(), { feature: event.currentTarget.value })}
                      />
                      <IconButton
                        label="Remove the feature"
                        icon="trash"
                        onClick={() => set({ features: draft().features.filter((_, at) => at !== index()) })}
                      />
                    </div>
                    <TextArea
                      label="Variables"
                      help="JSON. Leave it empty for the feature's defaults."
                      rows={3}
                      value={feature.vars}
                      onInput={(event) => setFeature(index(), { vars: event.currentTarget.value })}
                    />
                  </div>
                )}
              </For>
            </div>
          </Show>
        </Card>
      </form>
    </View>
  );
}

// A host's profile: what an apply makes of it. The form edits the parts a person
// changes by hand — where the host is, which type it is, the desktop of a
// workstation, its registries and the plugin features it installs — and sends the
// profile as a whole, which is the digest.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { cbor } from "@rookery/client";
import { HostType, type Host } from "@rookery/proto";

import { useShell } from "../../../shell.tsx";
import { Badge, Button, Callout, Field, IconButton, Input, Select, Switch } from "../../../ui.tsx";
import { errorMessage } from "../format.ts";
import { Dialog, TextArea } from "../parts.tsx";

/** FeatureDraft is one feature row, with its variables as JSON. */
interface FeatureDraft {
  readonly plugin: string;
  readonly feature: string;
  readonly vars: string;
}

export function HostProfile(props: {
  open: boolean;
  host: Host;
  onClose: () => void;
  onSaved: () => void;
}): JSX.Element {
  const shell = useShell();
  const profile = () => props.host.profile;
  const [type, setType] = createSignal(String(profile()?.type ?? HostType.SSH));
  const [address, setAddress] = createSignal(profile()?.address ?? "");
  const [port, setPort] = createSignal(String(profile()?.port ?? 22));
  const [sshUser, setSSHUser] = createSignal(profile()?.sshUser ?? "ops");
  const [operator, setOperator] = createSignal(profile()?.operator ?? "ops");
  const [tailnet, setTailnet] = createSignal(profile()?.tailnet ?? true);
  const [registries, setRegistries] = createSignal((profile()?.registries ?? []).join(", "));
  const [desktopUser, setDesktopUser] = createSignal(profile()?.desktop?.user ?? "dev");
  const [width, setWidth] = createSignal(String(profile()?.desktop?.width ?? 1920));
  const [height, setHeight] = createSignal(String(profile()?.desktop?.height ?? 1080));
  const [gpu, setGpu] = createSignal(profile()?.desktop?.gpu ?? false);
  const [features, setFeatures] = createSignal<FeatureDraft[]>(
    (profile()?.features ?? []).map(feature => ({
      plugin: feature.plugin,
      feature: feature.feature,
      vars: feature.varsCbor.length === 0 ? "" : JSON.stringify(cbor.decode(feature.varsCbor) ?? {}, undefined, 2),
    })),
  );
  const [busy, setBusy] = createSignal(false);
  const [failure, setFailure] = createSignal<string>();

  const problem = (): string | undefined => {
    if (address().trim() === "") return "The profile needs the address Rookery reaches the host at.";
    if (!Number.isInteger(Number(port())) || Number(port()) <= 0 || Number(port()) > 65535) {
      return "The SSH port is 1 to 65535.";
    }
    if (Number(type()) === HostType.WORKSTATION && (Number(width()) <= 0 || Number(height()) <= 0)) {
      return "The desktop's screen needs a width and a height.";
    }
    for (const feature of features()) {
      if (feature.plugin.trim() === "" || feature.feature.trim() === "") return "A feature names its plugin and the feature.";
      if (feature.vars.trim() !== "") {
        try {
          JSON.parse(feature.vars);
        } catch {
          return `${feature.plugin}/${feature.feature} has variables that are not JSON.`;
        }
      }
    }
    return undefined;
  };

  const save = async (): Promise<void> => {
    const message = problem();
    if (message !== undefined) {
      setFailure(message);
      return;
    }
    setBusy(true);
    setFailure(undefined);
    try {
      const made = features().map(feature => ({
        plugin: feature.plugin.trim(),
        feature: feature.feature.trim(),
        varsCbor:
          feature.vars.trim() === "" ? new Uint8Array() : cbor.encode(JSON.parse(feature.vars) as Record<string, unknown>),
      }));
      await shell.rookery.hosts.setProfile({
        name: props.host.name,
        profile: {
          type: Number(type()) as HostType,
          address: address().trim(),
          port: Number(port()),
          sshUser: sshUser().trim(),
          operator: operator().trim(),
          tailnet: tailnet(),
          registries: registries()
            .split(",")
            .map(name => name.trim())
            .filter(name => name !== ""),
          features: made,
          desktop:
            Number(type()) === HostType.WORKSTATION
              ? { user: desktopUser().trim(), width: Number(width()), height: Number(height()), gpu: gpu() }
              : undefined,
        },
      });
      props.onSaved();
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy(false);
    }
  };

  const change = (index: number, patch: Partial<FeatureDraft>): void => {
    setFeatures(previous => previous.map((feature, at) => (at === index ? { ...feature, ...patch } : feature)));
  };

  return (
    <Dialog
      open={props.open}
      onClose={props.onClose}
      title={`Profile of ${props.host.name}`}
      size="lg"
      description="What an apply makes of the host. A change leaves it behind until it is applied."
      footer={
        <>
          <Button onClick={props.onClose} disabled={busy()}>
            Cancel
          </Button>
          <Button variant="primary" onClick={() => void save()} disabled={busy()} data-part="save-profile">
            {busy() ? "Saving…" : "Save profile"}
          </Button>
        </>
      }
    >
      <Field label="Type">
        <Select
          value={type()}
          options={[
            { value: String(HostType.SSH), label: "SSH" },
            { value: String(HostType.MANAGED), label: "Managed" },
            { value: String(HostType.WORKSTATION), label: "Workstation" },
          ]}
          onChange={setType}
        />
      </Field>
      <Field label="Address" hint="Changing the address an existing host is reached at needs an admin.">
        <Input value={address()} onInput={setAddress} />
      </Field>
      <div class="nc-row">
        <Field label="SSH port">
          <Input value={port()} onInput={setPort} />
        </Field>
        <Field label="SSH user">
          <Input value={sshUser()} onInput={setSSHUser} />
        </Field>
        <Field label="Operator">
          <Input value={operator()} onInput={setOperator} />
        </Field>
      </div>
      <Switch label="Join the tailnet" checked={tailnet()} onChange={setTailnet} />
      <Field label="Registries" hint="Connection names, separated by commas. The apply writes their logins on the host.">
        <Input value={registries()} onInput={setRegistries} />
      </Field>
      <Show when={Number(type()) === HostType.WORKSTATION}>
        <div class="nc-row">
          <Field label="Desktop user">
            <Input value={desktopUser()} onInput={setDesktopUser} />
          </Field>
          <Field label="Width">
            <Input value={width()} onInput={setWidth} />
          </Field>
          <Field label="Height">
            <Input value={height()} onInput={setHeight} />
          </Field>
        </div>
        <Switch label="Give the desktop user the GPU" checked={gpu()} onChange={setGpu} />
      </Show>
      <section class="nc-stack">
        <h3 class="nc-section-heading">Features</h3>
        <For each={features()}>
          {(feature, index) => (
            <div class="infra-feature" data-part="profile-feature">
              <div class="nc-row">
                <Badge tone="outline">{feature.plugin === "" ? "plugin" : feature.plugin}</Badge>
                <span class="nc-code">{feature.feature}</span>
              </div>
              <TextArea
                label={`Variables of ${feature.plugin}/${feature.feature}`}
                rows={3}
                value={feature.vars}
                onInput={vars => change(index(), { vars })}
              />
              <IconButton
                label="Remove the feature"
                glyph="close"
                onClick={() => setFeatures(previous => previous.filter((_, at) => at !== index()))}
              />
            </div>
          )}
        </For>
        <Button
          size="sm"
          onClick={() => setFeatures(previous => [...previous, { plugin: "", feature: "", vars: "" }])}
        >
          Add feature
        </Button>
      </section>
      <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
    </Dialog>
  );
}

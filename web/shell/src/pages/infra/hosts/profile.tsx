// A host's profile: what an apply makes of it. The form edits the parts a person
// changes by hand — where the host is, which type it is, the desktop of a
// workstation, its registries and the plugin features it installs — and sends the
// profile as a whole, which is the digest.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { cbor } from "@rookery/client";
import { HostType, type Host } from "@rookery/proto";
import {
  Badge,
  Button,
  Callout,
  Dialog,
  IconButton,
  SectionHeading,
  Select,
  Switch,
  TextArea,
  TextField,
} from "@aiviarium/nocturne";

import { useShell } from "../../../shell.tsx";
import { errorMessage } from "../format.ts";

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
          <Button variant="primary" loading={busy()} onClick={() => void save()} data-part="save-profile">
            Save profile
          </Button>
        </>
      }
    >
      <div class="nc-stack">
      <Select
        label="Type"
        value={type()}
        options={[
          { value: String(HostType.SSH), label: "SSH" },
          { value: String(HostType.MANAGED), label: "Managed" },
          { value: String(HostType.WORKSTATION), label: "Workstation" },
        ]}
        onChange={setType}
      />
      <TextField
        label="Address"
        help="Changing the address an existing host is reached at needs an admin."
        value={address()}
        onInput={event => setAddress(event.currentTarget.value)}
      />
      <div class="nc-row">
        <TextField label="SSH port" value={port()} onInput={event => setPort(event.currentTarget.value)} />
        <TextField label="SSH user" value={sshUser()} onInput={event => setSSHUser(event.currentTarget.value)} />
        <TextField label="Operator" value={operator()} onInput={event => setOperator(event.currentTarget.value)} />
      </div>
      <Switch label="Join the tailnet" checked={tailnet()} onChange={setTailnet} />
      <TextField
        label="Registries"
        help="Connection names, separated by commas. The apply writes their logins on the host."
        value={registries()}
        onInput={event => setRegistries(event.currentTarget.value)}
      />
      <Show when={Number(type()) === HostType.WORKSTATION}>
        <div class="nc-row">
          <TextField label="Desktop user" value={desktopUser()} onInput={event => setDesktopUser(event.currentTarget.value)} />
          <TextField label="Width" value={width()} onInput={event => setWidth(event.currentTarget.value)} />
          <TextField label="Height" value={height()} onInput={event => setHeight(event.currentTarget.value)} />
        </div>
        <Switch label="Give the desktop user the GPU" checked={gpu()} onChange={setGpu} />
      </Show>
      <section class="nc-stack">
        <SectionHeading title="Features" level={3} />
        <For each={features()}>
          {(feature, index) => (
            <div class="shell-feature nc-stack" data-part="profile-feature">
              <div class="nc-row">
                <Badge tone="outline">{feature.plugin === "" ? "plugin" : feature.plugin}</Badge>
                <span class="nc-code">{feature.feature}</span>
                <IconButton
                  label="Remove the feature"
                  icon="close"
                  onClick={() => setFeatures(previous => previous.filter((_, at) => at !== index()))}
                />
              </div>
              <TextArea
                label={`Variables of ${feature.plugin}/${feature.feature}`}
                rows={3}
                value={feature.vars}
                onInput={event => change(index(), { vars: event.currentTarget.value })}
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
      </div>
    </Dialog>
  );
}

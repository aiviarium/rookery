// Keys: the ring the server logs in to its hosts with, and the host keys it
// trusts.
import { For, Show, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import { createQuery } from "@rookery/client/solid";

import { useShell } from "../../shell.tsx";
import { Badge, Button, Callout, Card, Empty, Input, Page, Spinner, Table } from "../../ui.tsx";

/** KeysPage is the key ring and the known hosts. */
export function KeysPage(): JSX.Element {
  const shell = useShell();
  const keys = createQuery(() => shell.rookery.keys.list({}));
  const known = createQuery(() => shell.rookery.keys.listKnownHosts({}));

  const [name, setName] = createSignal("");
  const [error, setError] = createSignal("");
  const [message, setMessage] = createSignal("");

  const rotate = async () => {
    setError("");
    setMessage("");
    try {
      const answer = await shell.rookery.keys.rotate({ name: name().trim() });
      setMessage(`The ring now uses ${answer.key?.fingerprint ?? "a new key"}.`);
      setName("");
      void keys.refresh();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  };

  return (
    <Page title="Keys" description="The SSH keys the server uses, and the host keys it trusts.">
      <Card
        title="Key ring"
        actions={
          <div class="nc-row nc-row--tight">
            <Input id="key-name" value={name()} placeholder="Name (default)" onInput={setName} />
            <Button size="sm" variant="primary" onClick={() => void rotate()}>
              Rotate
            </Button>
          </div>
        }
      >
        <div class="nc-stack">
          <Show when={message() !== ""}>
            <Callout tone="info">{message()}</Callout>
          </Show>
          <Show when={error() !== ""}>
            <Callout tone="danger">{error()}</Callout>
          </Show>
          <Show when={keys() !== undefined} fallback={<Spinner />}>
            <Table columns={["Name", "Type", "Fingerprint", "In use"]}>
              <For each={keys()?.keys ?? []}>
                {key => (
                  <tr>
                    <td>{key.name}</td>
                    <td>{key.type}</td>
                    <td class="nc-mono">{key.fingerprint}</td>
                    <td>
                      <Show when={key.active} fallback={<span class="nc-meta">retired</span>}>
                        <Badge tone="success">active</Badge>
                      </Show>
                    </td>
                  </tr>
                )}
              </For>
            </Table>
          </Show>
        </div>
      </Card>

      <Card title="Known hosts">
        <Show when={known() !== undefined} fallback={<Spinner />}>
          <Show
            when={(known()?.hosts.length ?? 0) > 0}
            fallback={<Empty title="No host keys yet" description="First contact records a host's key, and a changed key needs an admin's yes." />}
          >
            <Table columns={["Address", "Host", "Fingerprint", "Trust", ""]}>
              <For each={known()?.hosts ?? []}>
                {host => (
                  <tr>
                    <td class="nc-mono">{host.address}</td>
                    <td>{host.host}</td>
                    <td class="nc-mono">{host.pendingFingerprint !== "" ? host.pendingFingerprint : host.fingerprint}</td>
                    <td>
                      <Show when={host.pendingFingerprint !== ""} fallback={<Badge tone="success">trusted</Badge>}>
                        <Badge tone="warning">changed</Badge>
                      </Show>
                    </td>
                    <td>
                      <div class="nc-row nc-row--tight">
                        <Show when={host.pendingFingerprint !== ""}>
                          <Button
                            size="sm"
                            variant="primary"
                            onClick={() => {
                              void shell.rookery.keys
                                .trustHostKey({ address: host.address, fingerprint: host.pendingFingerprint })
                                .then(() => known.refresh());
                            }}
                          >
                            Trust the new key
                          </Button>
                        </Show>
                        <Button
                          size="sm"
                          variant="danger"
                          onClick={() => {
                            void shell.rookery.keys.forgetHostKey({ address: host.address }).then(() => known.refresh());
                          }}
                        >
                          Forget
                        </Button>
                      </div>
                    </td>
                  </tr>
                )}
              </For>
            </Table>
          </Show>
        </Show>
      </Card>
    </Page>
  );
}

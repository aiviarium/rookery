// "New space": a space of a kind a plugin declares. Making one opens it.
import { Show, createSignal, untrack } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Select, TextField } from "@aiviarium/nocturne";

import { createAction } from "../../parts/action.ts";
import { FormDialog } from "../../parts/dialogs.tsx";
import { useShell } from "../../shell.tsx";

/** NewSpace makes a space and hands its id on. */
export function NewSpace(props: { onClose: () => void; onCreated: (id: string) => void }): JSX.Element {
  const shell = useShell();
  const kinds = () => shell.spaces.kinds();
  // One kind needs no choice.
  const [kind, setKind] = createSignal(untrack(() => (kinds().length === 1 ? (kinds()[0]?.id ?? "") : "")));
  const [name, setName] = createSignal("");
  let created = "";
  const create = createAction(async () => {
    const answer = await shell.rookery.spaces.create({ kind: kind(), name: name().trim() });
    created = answer.space?.id ?? "";
  });
  return (
    <FormDialog
      open
      title="New space"
      submit="Create space"
      action={create}
      disabled={kind() === "" || name().trim() === ""}
      onClose={props.onClose}
      onDone={() => props.onCreated(created)}
    >
      <TextField label="Name" placeholder="Analysis" value={name()} onInput={(event) => setName(event.currentTarget.value)} />
      <Show when={kinds().length > 1}>
        <Select
          label="Kind"
          placeholder="Choose a kind"
          value={kind()}
          options={kinds().map((one) => ({ value: one.id, label: one.label, description: one.plugin === "" ? undefined : one.plugin }))}
          onChange={setKind}
        />
      </Show>
    </FormDialog>
  );
}

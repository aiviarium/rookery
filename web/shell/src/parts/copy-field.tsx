// A value a person copies, such as a sign-in link or a key: shown read-only in
// a field, with a button that puts it on the clipboard.
import type { JSX } from "@solidjs/web";
import { Button, TextField, toast } from "@aiviarium/nocturne";

/** CopyFieldProps are the value and what it is. */
export interface CopyFieldProps {
  readonly label: string;
  readonly value: string;
  readonly help?: string;
}

/** CopyField is a read-only field with a copy button. */
export function CopyField(props: CopyFieldProps): JSX.Element {
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(props.value);
      toast({ title: "Copied", tone: "success" });
    } catch {
      toast({ title: "The browser did not allow copying", description: "Select the text and copy it.", tone: "warning" });
    }
  };
  return (
    <div class="shell-form__row shell-form__row--end" data-part="copy-field">
      <TextField label={props.label} help={props.help} mono readonly value={props.value} onFocus={(event) => event.currentTarget.select()} />
      <Button icon="copy" onClick={() => void copy()}>
        Copy
      </Button>
    </div>
  );
}

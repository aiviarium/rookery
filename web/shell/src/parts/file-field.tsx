// A file the person picks, with a field's label and help.
//
// Nocturne has no file field: its fields take text (`TextField`, `TextArea`), a choice (`Select`) or a switch.
// Until it has a `FileField`, this is its field markup with a file input inside, so the control reads like the
// fields beside it.
import { createUniqueId } from "solid-js";
import type { JSX } from "@solidjs/web";

/** FileFieldProps is a labelled file input. */
export interface FileFieldProps {
  /** label says what the file is for. */
  readonly label: string;
  /** help is one line about the file, under the control. */
  readonly help?: string;
  /** accept limits the chooser, such as ".tar.gz,application/gzip". */
  readonly accept?: string;
  /** onPick receives the chosen file. */
  readonly onPick: (file: File) => void;
}

/** FileField is a file input under a field's label. */
export function FileField(props: FileFieldProps): JSX.Element {
  const id = createUniqueId();
  return (
    <div class="nc-field">
      <label class="nc-field__label" for={id}>
        {props.label}
      </label>
      <input
        id={id}
        class="nc-input"
        type="file"
        accept={props.accept}
        onChange={(event) => {
          const file = event.currentTarget.files?.[0];
          if (file !== undefined) props.onPick(file);
          event.currentTarget.value = "";
        }}
      />
      {props.help === undefined ? null : <span class="nc-field__help">{props.help}</span>}
    </div>
  );
}

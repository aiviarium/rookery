// The two dialogs every page uses: a form that makes or changes something, and
// a confirmation before something that cannot be undone.
//
// Both run an Action, so the submit button shows its spinner and a failure
// shows in the dialog, which stays open until the call succeeds.
import { Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Button, Callout, Dialog } from "@aiviarium/nocturne";

import type { Action } from "./action.ts";

/** FormDialogProps are a form in a dialog. */
export interface FormDialogProps {
  readonly open: boolean;
  readonly title: string;
  readonly description?: JSX.Element;
  /** submit is the button's label, such as "Add host". */
  readonly submit: string;
  /** action runs on submit; the dialog closes when it succeeds. */
  readonly action: Action<[]>;
  /** disabled holds the submit back while the form is not complete. */
  readonly disabled?: boolean;
  readonly size?: "sm" | "md" | "lg";
  readonly onClose: () => void;
  /** onDone runs after a successful submit, before the dialog closes. */
  readonly onDone?: () => void;
  readonly children?: JSX.Element;
}

/** FormDialog is a form whose submit runs an action. */
export function FormDialog(props: FormDialogProps): JSX.Element {
  const close = () => {
    props.action.reset();
    props.onClose();
  };
  const submit = async (event?: Event) => {
    event?.preventDefault();
    if (props.disabled) return;
    if (await props.action.run()) {
      props.onDone?.();
      close();
    }
  };
  return (
    <Dialog
      open={props.open}
      onClose={close}
      title={props.title}
      description={props.description}
      size={props.size ?? "md"}
      footer={
        <>
          <Button variant="ghost" onClick={close} disabled={props.action.busy()}>
            Cancel
          </Button>
          <Button
            variant="primary"
            loading={props.action.busy()}
            disabled={props.disabled}
            onClick={() => void submit()}
            data-part="dialog-submit"
          >
            {props.submit}
          </Button>
        </>
      }
    >
      <form class="shell-form" onSubmit={(event) => void submit(event)}>
        {props.children}
        <Show when={props.action.error()}>{(message) => <Callout tone="danger">{message()}</Callout>}</Show>
        {/* Enter in a field submits. */}
        <button type="submit" hidden aria-hidden="true" tabindex="-1" />
      </form>
    </Dialog>
  );
}

/** ConfirmDialogProps ask before an action. */
export interface ConfirmDialogProps {
  readonly open: boolean;
  readonly title: string;
  readonly description?: JSX.Element;
  /** confirm is the button's label, such as "Remove host". */
  readonly confirm: string;
  /** danger draws the confirm button red, for what cannot be undone. */
  readonly danger?: boolean;
  readonly action: Action<[]>;
  readonly onClose: () => void;
  readonly onDone?: () => void;
  readonly children?: JSX.Element;
}

/** ConfirmDialog asks once before running an action. */
export function ConfirmDialog(props: ConfirmDialogProps): JSX.Element {
  const close = () => {
    props.action.reset();
    props.onClose();
  };
  const confirm = async () => {
    if (await props.action.run()) {
      props.onDone?.();
      close();
    }
  };
  return (
    <Dialog
      open={props.open}
      onClose={close}
      title={props.title}
      description={props.description}
      size="sm"
      footer={
        <>
          <Button variant="ghost" onClick={close} disabled={props.action.busy()}>
            Cancel
          </Button>
          <Button
            variant={props.danger ? "danger" : "primary"}
            loading={props.action.busy()}
            onClick={() => void confirm()}
            data-part="dialog-confirm"
          >
            {props.confirm}
          </Button>
        </>
      }
    >
      <div class="shell-form">
        {props.children}
        <Show when={props.action.error()}>{(message) => <Callout tone="danger">{message()}</Callout>}</Show>
      </div>
    </Dialog>
  );
}

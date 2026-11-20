// A small busy indicator.
//
// Nocturne has the `.nc-spinner` styles and uses them in its own parts, but exports no component for them. Until
// it has a `Spinner`, this is the class with the status role a reader hears.
import type { JSX } from "@solidjs/web";

/** SpinnerProps names the work in progress. */
export interface SpinnerProps {
  /** label is what is loading, as a reader hears it. */
  readonly label?: string;
}

/** Spinner is work in progress. */
export function Spinner(props: SpinnerProps): JSX.Element {
  return <span class="nc-spinner" role="status" aria-label={props.label ?? "Loading"} />;
}

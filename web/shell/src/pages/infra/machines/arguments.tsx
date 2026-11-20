// A provisioner's arguments, as its description gives them: a string, a number, a
// duration, a size, a choice, a boolean or CBOR typed as JSON. The values become
// the CBOR map an entry, an action or a machine is made with.
import { For, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { cbor } from "@rookery/client";
import type { ProvisionerArgument } from "@rookery/proto";
import { Select, Switch, TextField } from "@aiviarium/nocturne";

/** ArgumentValues maps an argument's name to what the person typed. */
export type ArgumentValues = Record<string, string | boolean>;

/** argumentDefaults is the value each argument starts with. */
export function argumentDefaults(arguments_: readonly ProvisionerArgument[]): ArgumentValues {
  const values: ArgumentValues = {};
  for (const argument of arguments_) {
    const fallback = argument.defaultCbor.length === 0 ? undefined : cbor.decode(argument.defaultCbor);
    if (argument.type === "bool") values[argument.name] = fallback === true;
    else values[argument.name] = fallback === undefined || fallback === null ? "" : String(fallback);
  }
  return values;
}

/** argumentsCbor encodes the values as the CBOR map the server reads, dropping empty ones. */
export function argumentsCbor(arguments_: readonly ProvisionerArgument[], values: ArgumentValues): Uint8Array {
  const map: Record<string, unknown> = {};
  for (const argument of arguments_) {
    const value = values[argument.name];
    if (value === undefined || value === "" || value === false) continue;
    switch (argument.type) {
      case "int":
      case "size":
        map[argument.name] = Number(value);
        break;
      case "duration":
        // A duration is minutes in the field and seconds on the wire.
        map[argument.name] = Math.round(Number(value) * 60);
        break;
      case "bool":
        map[argument.name] = value === true;
        break;
      case "cbor":
        map[argument.name] = JSON.parse(String(value)) as unknown;
        break;
      default:
        map[argument.name] = value;
    }
  }
  return cbor.encode(map);
}

/** argumentProblems says which required arguments are missing. */
export function argumentProblems(
  arguments_: readonly ProvisionerArgument[],
  values: ArgumentValues,
): Record<string, string> {
  const problems: Record<string, string> = {};
  for (const argument of arguments_) {
    if (!argument.required || argument.secret) continue;
    const value = values[argument.name];
    if (value === undefined || value === "" || value === false) problems[argument.name] = "Needed.";
  }
  return problems;
}

/** ArgumentFields draws one field per argument a provisioner takes. */
export function ArgumentFields(props: {
  arguments: readonly ProvisionerArgument[];
  values: ArgumentValues;
  errors: Record<string, string>;
  onChange: (name: string, value: string | boolean) => void;
}): JSX.Element {
  return (
    <For each={props.arguments.filter(argument => !argument.secret)}>
      {argument => {
        const label = argument.label === "" ? argument.name : argument.label;
        const help = [argument.help, argument.type === "duration" ? "In minutes." : ""]
          .filter(part => part !== "")
          .join(" ");
        return (
          <Show
            when={argument.type === "bool"}
            fallback={
              <Show
                when={argument.type === "select" && argument.choices.length > 0}
                fallback={
                  <TextField
                    label={label}
                    help={help === "" ? undefined : help}
                    error={props.errors[argument.name]}
                    value={String(props.values[argument.name] ?? "")}
                    placeholder={argument.type === "cbor" ? "{}" : undefined}
                    onInput={event => props.onChange(argument.name, event.currentTarget.value)}
                  />
                }
              >
                <Select
                  label={label}
                  help={help === "" ? undefined : help}
                  value={String(props.values[argument.name] ?? "")}
                  options={argument.choices.map(choice => ({ value: choice, label: choice }))}
                  onChange={chosen => props.onChange(argument.name, chosen)}
                />
              </Show>
            }
          >
            <Switch
              label={label}
              checked={props.values[argument.name] === true}
              onChange={checked => props.onChange(argument.name, checked)}
            />
          </Show>
        );
      }}
    </For>
  );
}

// The "more" button at the end of a row: the row's actions in a menu, so a row
// shows what it is and not a strip of buttons. A right click on a ListRow can
// open the same entries through its `menu`.
import type { JSX } from "@solidjs/web";
import { DropdownMenu, IconButton, type MenuEntry } from "@aiviarium/nocturne";

/** RowActionsProps are a row's actions. */
export interface RowActionsProps {
  /** label names the menu after the row, such as "Actions for forge". */
  readonly label: string;
  readonly items: MenuEntry[];
  readonly onSelect: (id: string) => void;
}

/** RowActions is a row's menu behind a "more" button. */
export function RowActions(props: RowActionsProps): JSX.Element {
  return (
    <DropdownMenu
      label={props.label}
      placement="bottom-end"
      items={props.items}
      onSelect={props.onSelect}
      trigger={(trigger) => <IconButton {...trigger} size="sm" icon="more" label={props.label} />}
    />
  );
}

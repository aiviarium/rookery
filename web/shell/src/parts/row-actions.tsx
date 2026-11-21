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

/**
 * RowActions is a row's menu behind a "more" button. The menu draws in a
 * portal, whose clicks still bubble to the row through Solid's tree, so they
 * stop here: picking an item never also opens the row.
 */
export function RowActions(props: RowActionsProps): JSX.Element {
  return (
    <span class="shell-row-actions" onClick={(event) => event.stopPropagation()}>
      <DropdownMenu
        label={props.label}
        placement="bottom-end"
        items={props.items}
        onSelect={props.onSelect}
        trigger={(trigger) => <IconButton {...trigger} size="sm" icon="more" label={props.label} />}
      />
    </span>
  );
}

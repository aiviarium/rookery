// The frame of every page: Nocturne's ViewLayout with a ViewHeader.
//
// The header holds the path and the page's actions; the page never repeats its
// name as a heading. While the person hid the sidebar, Nocturne's shell puts
// the toggle that brings it back at the start of the header.
import { Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Callout, ViewHeader, ViewLayout, type ViewAction, type ViewCrumb } from "@aiviarium/nocturne";

/** ViewProps are what a page shows around its content. */
export interface ViewProps {
  /** id keeps the view's column widths on the device. */
  readonly id: string;
  /** crumbs are the path to the page; the last is the page. */
  readonly crumbs: ViewCrumb[];
  /** back replaces the crumbs with a back control, for a page reached from one list. */
  readonly back?: ViewCrumb;
  /** context is a short state beside the path, such as a host's state. */
  readonly context?: JSX.Element;
  /** actions are the page's, in Nocturne's order: icons, overflow, then the primary one. */
  readonly actions?: ViewAction[];
  /** error is a failure that stops the page from showing what it holds. */
  readonly error?: string;
  /** aside is the inspector beside the main column. */
  readonly aside?: JSX.Element;
  /** asideLabel names the inspector. */
  readonly asideLabel?: string;
  /** reading caps the main column's width, for pages of prose and forms. */
  readonly reading?: boolean;
  /** scroll false lets a page such as a terminal fill the frame itself. */
  readonly scroll?: boolean;
  readonly children?: JSX.Element;
}

/** View is one page's frame. */
export function View(props: ViewProps): JSX.Element {
  return (
    <ViewLayout
      id={props.id}
      header={<ViewHeader crumbs={props.crumbs} back={props.back} context={props.context} actions={props.actions} noPin />}
      aside={props.aside}
      asideLabel={props.asideLabel}
      reading={props.reading}
      scroll={props.scroll}
    >
      <div class="shell-view" data-view={props.id}>
        <Show when={props.error}>{(message) => <Callout tone="danger">{message()}</Callout>}</Show>
        {props.children}
      </div>
    </ViewLayout>
  );
}

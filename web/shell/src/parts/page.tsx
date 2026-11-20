// The frame of a page: the view header with the path and the page's actions, one line about the page, and its
// content.
//
// Nocturne has no part for a whole page that is not a view with a sidebar of its own: `ViewLayout` lays out the
// columns of such a view, and `ViewHeader` is the header inside it. A page of the shell has no view sidebar, so
// this composes the two pieces a page needs from Nocturne, and adds nothing of its own beyond the one line under
// the path. Nocturne would need a `Page` that is a `ViewHeader` over a scrolling main column without a sidebar.
import { Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { ViewHeader, type ViewAction, type ViewCrumb } from "@aiviarium/nocturne";

/** PageProps is what a page shows above its content. */
export interface PageProps {
  /** title is the page's name, the last crumb of its path. */
  readonly title: string;
  /** crumbs lead to the page, from a place that opens it; the title ends them. */
  readonly crumbs?: () => ViewCrumb[];
  /** description is one line about what the page holds. */
  readonly description?: string;
  /** context is a short state beside the path. */
  readonly context?: JSX.Element;
  /** actions are what the page lets a person do, in Nocturne's order. */
  readonly actions?: () => ViewAction[];
  readonly children?: JSX.Element;
}

/** Page is the header and content of one page of the shell. */
export function Page(props: PageProps): JSX.Element {
  const crumbs = () => props.crumbs?.() ?? [{ label: props.title }];
  return (
    <div class="shell-page nc-stack">
      <ViewHeader crumbs={crumbs()} context={props.context} actions={props.actions?.()} noPin />
      <Show when={props.description !== undefined}>
        <p class="nc-muted shell-page__description">{props.description}</p>
      </Show>
      {props.children}
    </div>
  );
}

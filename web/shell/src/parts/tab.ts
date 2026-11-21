// The tab a page shows, kept in the address as ?tab=, so a reload or a link
// lands on the same one.
import { useSearchParams } from "@solidjs/router";

/** createTab is the selected tab of a page, the first one when the address names none. */
export function createTab<T extends string>(tabs: readonly T[]): [() => T, (tab: T) => void] {
  const [search, setSearch] = useSearchParams<{ tab?: string }>();
  const tab = (): T => (tabs.includes(search.tab as T) ? (search.tab as T) : tabs[0]!);
  return [tab, (next) => setSearch({ tab: next === tabs[0] ? undefined : next }, { replace: true })];
}

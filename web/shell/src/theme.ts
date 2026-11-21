// The theme the person chose: dark, light or the system's. It is written to
// `data-theme` on the document, where Nocturne's tokens read it, and kept in
// the browser.
import { type Accessor, createSignal, untrack } from "solid-js";

/** ThemeChoice is what the person picked. */
export type ThemeChoice = "system" | "dark" | "light";

/** THEME_KEY is where the browser keeps the choice. */
const THEME_KEY = "rookery-theme";

/** Theme is the choice and the way to change it. */
export interface Theme {
  readonly choice: Accessor<ThemeChoice>;
  set(choice: ThemeChoice): void;
}

/** createTheme reads the stored choice, applies it and follows the system while it is "system". */
export function createTheme(): Theme {
  const [choice, setChoice] = createSignal<ThemeChoice>(readStored());
  const media = typeof matchMedia === "function" ? matchMedia("(prefers-color-scheme: light)") : undefined;
  const apply = (next: ThemeChoice) => {
    if (typeof document === "undefined") return;
    const shown = next === "system" ? (media?.matches ? "light" : "dark") : next;
    document.documentElement.dataset.theme = shown;
  };
  apply(untrack(choice));
  media?.addEventListener("change", () => {
    if (untrack(choice) === "system") apply("system");
  });
  return {
    choice,
    set(next) {
      setChoice(next);
      apply(next);
      try {
        localStorage.setItem(THEME_KEY, next);
      } catch {
        // A browser without storage keeps the choice for this page only.
      }
    },
  };
}

function readStored(): ThemeChoice {
  try {
    const stored = localStorage.getItem(THEME_KEY);
    if (stored === "dark" || stored === "light" || stored === "system") return stored;
  } catch {
    // No storage: the default below.
  }
  return "system";
}

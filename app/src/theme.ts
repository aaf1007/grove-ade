import { useLayoutEffect, useSyncExternalStore } from "react";

import { hasShape, useSetting } from "./settings";

export type Theme = "system" | "light" | "dark";
export type Palette = "default" | "modern";
export type ColorScheme = "light" | "dark";

export interface Appearance {
  theme: Theme;
  palette: Palette;
}

export const THEMES: [Theme, string][] = [
  ["system", "System"],
  ["light", "Light"],
  ["dark", "Dark"],
];

export const PALETTES: [Palette, string][] = [
  ["default", "Default"],
  ["modern", "Modern"],
];

const DEFAULT_APPEARANCE: Appearance = { theme: "system", palette: "default" };

const isPalette = (palette: unknown) => PALETTES.some(([known]) => known === palette);

const isAppearance = (value: unknown): value is Appearance =>
  hasShape(value, {
    theme: (theme) => THEMES.some(([known]) => known === theme),
    palette: isPalette,
  });

// A saved palette id that is not known gives the default palette, and the saved theme stays.
const withKnownPalette = (saved: unknown) =>
  hasShape(saved, { palette: (palette) => !isPalette(palette) })
    ? { ...saved, palette: DEFAULT_APPEARANCE.palette }
    : saved;

/**
 * The saved theme and palette, applied to the root element as `data-theme` and `data-palette`.
 * The System theme resolves to light or dark here and follows the OS as it changes.
 */
export function useAppearance() {
  const [appearance, setAppearance] = useSetting<Appearance>(
    "grove.appearance",
    DEFAULT_APPEARANCE,
    isAppearance,
    withKnownPalette,
  );
  const systemScheme = useSyncExternalStore(subscribeToSystemScheme, readSystemScheme);
  const scheme = appearance.theme === "system" ? systemScheme : appearance.theme;

  // Before paint, so the window never shows the wrong palette.
  useLayoutEffect(() => {
    const root = document.documentElement.dataset;
    root.theme = scheme;
    root.palette = appearance.palette;
  }, [scheme, appearance.palette]);

  return { appearance, setAppearance };
}

const DARK = "(prefers-color-scheme: dark)";

function subscribeToSystemScheme(changed: () => void) {
  const query = matchMedia(DARK);
  query.addEventListener("change", changed);
  return () => query.removeEventListener("change", changed);
}

function readSystemScheme(): ColorScheme {
  return matchMedia(DARK).matches ? "dark" : "light";
}

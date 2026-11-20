import { useLayoutEffect, useSyncExternalStore } from "react";

import type { DockIcon } from "./bridge";
import { hasShape, useSetting } from "./settings";

export type Theme = "system" | "light" | "dark";
export type Palette = "default" | "modern";
export type ColorScheme = "light" | "dark";
/** The app icon's style in the Dock. Auto uses Mono. */
export type AppIcon = "auto" | "mono" | "blue";

export interface Appearance {
  theme: Theme;
  palette: Palette;
  appIcon: AppIcon;
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

export const APP_ICONS: [AppIcon, string][] = [
  ["auto", "Auto"],
  ["mono", "Mono"],
  ["blue", "Blue"],
];

const DEFAULT_APPEARANCE: Appearance = { theme: "system", palette: "default", appIcon: "auto" };

const isPalette = (palette: unknown) => PALETTES.some(([known]) => known === palette);
const isAppIcon = (appIcon: unknown) => APP_ICONS.some(([known]) => known === appIcon);

const isAppearance = (value: unknown): value is Appearance =>
  hasShape(value, {
    theme: (theme) => THEMES.some(([known]) => known === theme),
    palette: isPalette,
    appIcon: isAppIcon,
  });

// A saved palette or app icon that is not known, or missing as in an appearance saved before the
// app icon setting, gives its default, and the saved theme stays.
const withKnownChoices = (saved: unknown) =>
  hasShape(saved, {})
    ? {
        ...saved,
        palette: isPalette(saved.palette) ? saved.palette : DEFAULT_APPEARANCE.palette,
        appIcon: isAppIcon(saved.appIcon) ? saved.appIcon : DEFAULT_APPEARANCE.appIcon,
      }
    : saved;

/**
 * The saved theme and palette, applied to the root element as `data-theme` and `data-palette`.
 * The System theme resolves to light or dark here and follows the OS as it changes. `dockIcon` is
 * the app icon's style in the version that matches the resolved theme.
 */
export function useAppearance() {
  const [appearance, setAppearance] = useSetting<Appearance>(
    "grove.appearance",
    DEFAULT_APPEARANCE,
    isAppearance,
    withKnownChoices,
  );
  const systemScheme = useSyncExternalStore(subscribeToSystemScheme, readSystemScheme);
  const scheme = appearance.theme === "system" ? systemScheme : appearance.theme;

  // Before paint, so the window never shows the wrong palette.
  useLayoutEffect(() => {
    const root = document.documentElement.dataset;
    root.theme = scheme;
    root.palette = appearance.palette;
  }, [scheme, appearance.palette]);

  const dockIcon: DockIcon = `${appearance.appIcon === "blue" ? "blue" : "mono"}-${scheme}`;

  return { appearance, setAppearance, dockIcon };
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

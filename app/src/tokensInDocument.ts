import tokens from "./tokens.css?raw";

/**
 * For tests: puts `css`, the token file by default, in the document as the app's stylesheet does,
 * and sets the root's theme and palette. Returns a function that takes it out again.
 */
export function tokensInDocument(
  { theme = "dark", palette = "default", css = tokens }: { theme?: string; palette?: string; css?: string } = {},
) {
  const style = document.createElement("style");
  style.textContent = css;
  document.head.append(style);
  setAppearance(theme, palette);
  return () => {
    style.remove();
    delete document.documentElement.dataset.theme;
    delete document.documentElement.dataset.palette;
  };
}

/** Sets the root's theme and palette, as `useAppearance` does. */
export function setAppearance(theme: string, palette: string) {
  document.documentElement.dataset.theme = theme;
  document.documentElement.dataset.palette = palette;
}

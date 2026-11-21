import { useSyncExternalStore } from "react";

import { hasShape, useSetting } from "./settings";

export const SIDEBAR_MIN_WIDTH = 200;
export const SIDEBAR_MAX_WIDTH = 380;
export const PANELS_MIN_WIDTH = 300;
// The chat area keeps this much, so the panel area can grow only into the rest.
export const CHAT_MIN_WIDTH = 300;
export const SPLITTER_WIDTH = 5;

/** Which regions the window shows, and how wide. Saved as an app setting for now (#121). */
export interface Layout {
  sidebarOpen: boolean;
  panelsOpen: boolean;
  sidebarWidth: number;
  panelsWidth: number;
}

const DEFAULT_LAYOUT: Layout = {
  sidebarOpen: true,
  panelsOpen: true,
  sidebarWidth: 256,
  panelsWidth: 720,
};

const isBoolean = (value: unknown) => typeof value === "boolean";
const isWidth = (value: unknown) => typeof value === "number" && Number.isFinite(value);
const isLayout = (value: unknown): value is Layout =>
  hasShape(value, {
    sidebarOpen: isBoolean,
    panelsOpen: isBoolean,
    sidebarWidth: isWidth,
    panelsWidth: isWidth,
  });

/** The saved layout, with each width kept inside its region's bounds at the window's width. */
export function useLayout() {
  const [layout, setLayout] = useSetting("grove.layout", DEFAULT_LAYOUT, isLayout);
  const windowWidth = useSyncExternalStore(subscribeToWindowWidth, () => window.innerWidth);

  const sidebar = bounded(layout.sidebarWidth, SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
  const sidebarSpace = layout.sidebarOpen ? sidebar.width + SPLITTER_WIDTH : 0;
  const panels = bounded(
    layout.panelsWidth,
    PANELS_MIN_WIDTH,
    windowWidth - sidebarSpace - CHAT_MIN_WIDTH - SPLITTER_WIDTH,
  );

  return {
    layout,
    sidebar,
    panels,
    toggleSidebar: () => setLayout((l) => ({ ...l, sidebarOpen: !l.sidebarOpen })),
    togglePanels: () => setLayout((l) => ({ ...l, panelsOpen: !l.panelsOpen })),
    resizeSidebar: (width: number) => setLayout((l) => ({ ...l, sidebarWidth: width })),
    resizePanels: (width: number) => setLayout((l) => ({ ...l, panelsWidth: width })),
  };
}

/** A region's width and bounds. The minimum wins when the window is too narrow for both. */
export interface Bounded {
  width: number;
  min: number;
  max: number;
}

function bounded(width: number, min: number, max: number): Bounded {
  const top = Math.max(min, max);
  return { width: Math.min(top, Math.max(min, width)), min, max: top };
}

function subscribeToWindowWidth(changed: () => void) {
  window.addEventListener("resize", changed);
  return () => window.removeEventListener("resize", changed);
}

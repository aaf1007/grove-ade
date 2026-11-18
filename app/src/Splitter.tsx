import { useEffect, useRef, useState, type KeyboardEvent, type PointerEvent } from "react";

import type { Bounded } from "./layout";

const KEY_STEP = 16;

/**
 * A vertical splitter that resizes the region on its `side`. Dragging it or pressing the arrow
 * keys moves it, and the region's width never leaves its bounds.
 */
export function Splitter({
  label,
  side,
  region,
  onResize,
}: {
  label: string;
  /** Where the region it resizes sits: before it (to its left) or after it. */
  side: "before" | "after";
  region: Bounded;
  onResize(width: number): void;
}) {
  const [dragging, setDragging] = useState(false);
  const stopDragging = useRef<(() => void) | null>(null);
  // Moving right grows a region on the left and shrinks one on the right.
  const direction = side === "before" ? 1 : -1;
  const resize = (width: number) =>
    onResize(Math.min(region.max, Math.max(region.min, Math.round(width))));

  useEffect(() => () => stopDragging.current?.(), []);

  function startDragging(event: PointerEvent) {
    if (event.button !== 0) return;
    // Keeps the drag from selecting text.
    event.preventDefault();
    const startX = event.clientX;
    const startWidth = region.width;
    const move = (e: globalThis.PointerEvent) =>
      resize(startWidth + (e.clientX - startX) * direction);
    const stop = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", stop);
      stopDragging.current = null;
      setDragging(false);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", stop);
    stopDragging.current = stop;
    setDragging(true);
  }

  function moveByKey(event: KeyboardEvent) {
    const step = { ArrowLeft: -KEY_STEP, ArrowRight: KEY_STEP }[event.key];
    if (step === undefined) return;
    event.preventDefault();
    resize(region.width + step * direction);
  }

  return (
    <div
      className={`splitter split-${side}${dragging ? " dragging" : ""}`}
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      aria-valuenow={region.width}
      aria-valuemin={region.min}
      aria-valuemax={region.max}
      tabIndex={0}
      onPointerDown={startDragging}
      onKeyDown={moveByKey}
    />
  );
}

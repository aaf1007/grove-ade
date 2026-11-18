import { useEffect, useState } from "react";

/**
 * An app setting kept in the webview's storage, so it survives a reload and a restart. A saved
 * value that `valid` rejects, or that is not JSON, gives `initial`. `repair` can first replace a
 * part of the saved value, so one bad field does not cost the rest.
 */
export function useSetting<T>(
  key: string,
  initial: T,
  valid: (value: unknown) => value is T,
  repair: (saved: unknown) => unknown = (saved) => saved,
): [T, (update: (value: T) => T) => void] {
  const [value, setValue] = useState(() => {
    const saved = repair(load(key));
    return valid(saved) ? saved : initial;
  });
  // Storage can be full or blocked. The setting then lasts until the window closes.
  useEffect(() => {
    try {
      localStorage.setItem(key, JSON.stringify(value));
    } catch (e) {
      console.warn(`could not save the ${key} setting:`, e);
    }
  }, [key, value]);
  return [value, setValue];
}

function load(key: string): unknown {
  try {
    return JSON.parse(localStorage.getItem(key) ?? "null");
  } catch {
    return null;
  }
}

/** Whether `value` is an object whose `keys` all pass their checks. */
export function hasShape(
  value: unknown,
  checks: Record<string, (field: unknown) => boolean>,
): value is Record<string, unknown> {
  if (typeof value !== "object" || value === null) return false;
  const fields = value as Record<string, unknown>;
  return Object.entries(checks).every(([key, check]) => check(fields[key]));
}

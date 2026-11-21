// A popover plays its enter transition only when a pointer opened it. Opened from the keyboard or
// by a shortcut, it appears at once. The last input before it opens decides.
let lastInputWasPointer = false;
document.addEventListener("pointerdown", () => (lastInputWasPointer = true), true);
document.addEventListener("keydown", () => (lastInputWasPointer = false), true);

/** Whether the last input was a pointer rather than a key. Read it when a popover opens. */
export function openedByPointer(): boolean {
  return lastInputWasPointer;
}

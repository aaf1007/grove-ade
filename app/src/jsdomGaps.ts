// Runs before each test file. jsdom has no matchMedia, and Monaco's services read the OS's
// contrast setting through it when they start, which they do in any test that colors a code block.
// A test that needs a color scheme stubs its own.
window.matchMedia ??= (media: string) =>
  ({ matches: false, media, addEventListener() {}, removeEventListener() {} }) as unknown as MediaQueryList;

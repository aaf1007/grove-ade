import { useEffect, useMemo, useRef, useState } from "react";

import { colorScheme, onAppearanceChange, readToken } from "./codeTheme";
import "./InlinePage.css";

/**
 * What a page may load: nothing from anywhere. Scripts and styles run only inline, and images,
 * fonts and media come only from `data:` and `blob:` URLs the page makes itself. It is
 * `INLINE_PAGE_CSP` in `crates/grove-core/src/inline_page.rs` without its `sandbox`, which a
 * `<meta>` cannot set: the frame's `sandbox` attribute does that. A test in
 * `app/src-tauri/src/main.rs` keeps the two the same.
 */
export const PAGE_POLICY =
  "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; media-src data: blob:; form-action 'none'; base-uri 'none'";

/** The tallest a page's frame gets, in CSS pixels. Past it the frame scrolls. */
const MAX_HEIGHT = 2000;
/** The frame's height until its page says how tall it is. */
const UNMEASURED_HEIGHT = 160;

/** The CSS variables a page gets, and the token each takes its value from. */
const THEME_VARIABLES: [variable: string, token: string][] = [
  ["background", "panel"],
  ["foreground", "fg"],
  ["muted", "muted"],
  ["border", "line"],
  ["accent", "accent"],
  ["chart-1", "working"],
  ["chart-2", "done"],
  ["chart-3", "needs"],
  ["chart-4", "question"],
  ["chart-5", "failed"],
  ["chart-6", "claude"],
  ["radius", "radius-md"],
  ["font-sans", "font-ui"],
  ["font-mono", "font-mono"],
  ["font-size", "text-body-size"],
];

/** grove's theme for a page, as the root has it now: one rule that sets every variable. */
export function pageTheme(): string {
  const variables = THEME_VARIABLES.map(([variable, token]) => `--${variable}:${readToken(token)};`);
  return `:root{color-scheme:${colorScheme()};${variables.join("")}}`;
}

// What a page looks like before its own styles: grove's surface, text color and font.
const BASE_STYLE =
  "body{margin:0;background:var(--background);color:var(--foreground);font-family:var(--font-sans);font-size:var(--font-size)}";

// Runs in the page, before the agent's HTML. It takes a new theme from the window that holds the
// frame and from no other, and tells that window how tall the page is whenever that changes.
const PAGE_SCRIPT = [
  "(function(){",
  'var theme=document.getElementById("grove-theme");',
  'addEventListener("message",function(e){',
  'if(e.source!==parent||!e.data||e.data.type!=="grove-page-theme")return;',
  "theme.textContent=String(e.data.css)});",
  "function report(){",
  "var height=Math.ceil(document.documentElement.getBoundingClientRect().height);",
  'parent.postMessage({type:"grove-page-height",height:height},"*")}',
  "new ResizeObserver(report).observe(document.documentElement);",
  'addEventListener("load",report)',
  "})()",
].join("");

/**
 * The document a page's frame shows: the policy first, so it covers everything after it, then
 * `theme`, then the agent's `html` as the agent wrote it.
 */
export function pageDocument(html: string, theme: string): string {
  return [
    '<!doctype html><meta charset="utf-8">',
    `<meta http-equiv="Content-Security-Policy" content="${PAGE_POLICY}">`,
    `<style id="grove-theme">${theme}</style>`,
    `<style>${BASE_STYLE}</style>`,
    `<script>${PAGE_SCRIPT}</script>`,
    html,
  ].join("");
}

function messageOf(data: unknown): { type?: unknown; height?: unknown } {
  return typeof data === "object" && data !== null ? data : {};
}

/**
 * An inline page: HTML an agent showed in its conversation, under its title (ADR 0018). The page
 * is the agent's own code, so it runs in a frame that only runs scripts: it has its own origin,
 * loads nothing, and reaches neither grove nor the window.
 *
 * `frame` is the URL the app serves the page's frame from, under the page policy. The window
 * sends the page to it once it says it is ready. null where nothing serves one, as on the fake
 * bridge: the frame then holds the page itself. `height` is the frame's height in CSS pixels,
 * or null to fit the page.
 */
export function InlinePage({
  title,
  html,
  height,
  frame,
}: {
  title: string;
  html: string;
  height: number | null;
  frame: string | null;
}) {
  const iframe = useRef<HTMLIFrameElement>(null);
  const [fitted, setFitted] = useState(UNMEASURED_HEIGHT);
  // Built once per page. A later theme reaches the page as a message, so it is not loaded again
  // and keeps what the user did in it.
  const held = useMemo(() => (frame === null ? pageDocument(html, pageTheme()) : undefined), [frame, html]);

  useEffect(() => {
    function onMessage(event: MessageEvent) {
      const page = iframe.current?.contentWindow;
      // What the page sends is the agent's word, so only these two messages count.
      if (!page || event.source !== page) return;
      const message = messageOf(event.data);
      if (message.type === "grove-page-ready" && frame !== null) {
        // The page's origin is its own and has no name, so "*" is the only target that reaches it.
        page.postMessage({ type: "grove-page", document: pageDocument(html, pageTheme()) }, "*");
      }
      const tall = message.height;
      if (message.type === "grove-page-height" && typeof tall === "number" && Number.isFinite(tall) && tall >= 0) {
        setFitted(Math.min(tall, MAX_HEIGHT));
      }
    }
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [frame, html]);

  useEffect(
    () =>
      onAppearanceChange(() =>
        iframe.current?.contentWindow?.postMessage({ type: "grove-page-theme", css: pageTheme() }, "*"),
      ),
    [],
  );

  return (
    <div className="card inline-page" role="group" aria-label={`Page: ${title}`}>
      <div className="card-head">
        <b>{title}</b>
      </div>
      <iframe
        ref={iframe}
        title={title}
        // Scripts and nothing else. Without allow-same-origin the page cannot read the window.
        sandbox="allow-scripts"
        src={frame ?? undefined}
        srcDoc={held}
        style={{ height: Math.min(height ?? fitted, MAX_HEIGHT) }}
      />
    </div>
  );
}

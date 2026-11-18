import { Copy } from "lucide-react";
import { memo, type ReactNode, useRef } from "react";
import Markdown, { type Components, defaultUrlTransform, type UrlTransform } from "react-markdown";
import remarkGfm from "remark-gfm";

import { IconButton } from "./ui/IconButton";

/**
 * Agent text in chat mode as markdown. Raw HTML in it shows as text. It parses again only when its
 * text changes, so a long conversation costs nothing while the next reply streams.
 */
export const AgentMarkdown = memo(function AgentMarkdown({
  text,
  onOpenLink,
}: {
  text: string;
  onOpenLink(link: string): void;
}) {
  const components: Components = {
    pre: CodeBlock,
    // grove's window never navigates: the link opens in the user's browser.
    // The link as the agent wrote it goes to the core, which refuses any but http and https and
    // says why.
    a: ({ node, href, title, children }) => (
      <a
        // A blanked link keeps "#", so it is still a link and takes the keyboard.
        href={href || "#"}
        title={title}
        onClick={(event) => {
          event.preventDefault();
          const link = (node && linksAsWritten.get(node)) ?? href;
          if (link) onOpenLink(link);
        }}
      >
        {children}
      </a>
    ),
  };
  return (
    <div className="msg-agent">
      <Markdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[dropNewlineAfterBreak]}
        urlTransform={keepLinkAsWritten}
        components={components}
      >
        {text}
      </Markdown>
    </div>
  );
});

// The default transform blanks a link of any scheme it does not trust, such as file: or
// javascript:, so the DOM never holds one. Each link element keeps what the agent wrote here.
const linksAsWritten = new WeakMap<object, string>();

const keepLinkAsWritten: UrlTransform = (url, key, node) => {
  if (key === "href") linksAsWritten.set(node, url);
  return defaultUrlTransform(url);
};

/** The part of a hast node this plugin reads. */
type HastNode = { type: string; tagName?: string; value?: string; children?: HastNode[] };

// A hard break comes out as <br> then "\n". The stylesheet shows a newline in a paragraph as a
// break, so the "\n" goes, or the break would show as an empty line.
function dropNewlineAfterBreak() {
  return function drop(node: HastNode) {
    node.children?.forEach((child, i) => {
      const before = node.children![i - 1];
      if (child.type === "text" && before?.tagName === "br") child.value = child.value!.replace(/^\n/, "");
      drop(child);
    });
  };
}

// A fenced code block, which scrolls sideways when wide, with a button that copies its code.
function CodeBlock({ children }: { children?: ReactNode }) {
  const pre = useRef<HTMLPreElement>(null);
  return (
    <div className="code-block">
      <pre ref={pre}>{children}</pre>
      <IconButton
        icon={Copy}
        label="Copy code"
        size="xs"
        className="code-block-copy"
        onClick={() => void navigator.clipboard.writeText(pre.current!.textContent)}
      />
    </div>
  );
}

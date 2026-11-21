import { Copy } from "lucide-react";
import { Fragment, memo, useEffect, useState } from "react";
import Markdown, { type Components, defaultUrlTransform, type UrlTransform } from "react-markdown";
import remarkGfm from "remark-gfm";

import { type CodeRun, colorRuns } from "./monaco";
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
        remarkPlugins={[remarkGfm, markOpenFences]}
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

/** The part of a hast node these plugins and the code block read. */
type HastNode = {
  type: string;
  tagName?: string;
  value?: string;
  properties?: Record<string, unknown>;
  children?: HastNode[];
};

/** The part of an mdast node `markOpenFences` reads and writes. */
type MdastNode = {
  type: string;
  value?: string;
  position?: { start: { offset?: number }; end: { offset?: number } };
  data?: { hProperties?: Record<string, unknown> };
  children?: MdastNode[];
};

// While a reply streams, a fenced block can be open: its closing fence has not arrived, or only
// part of it has. The block is marked open, so it is not colored while it grows, and the part of a
// closing fence that has arrived leaves its code.
function markOpenFences() {
  return (tree: MdastNode, file: { value: unknown }) => {
    const markdown = String(file.value);
    const mark = (node: MdastNode) => {
      node.children?.forEach(mark);
      if (node.type !== "code") return;
      const source = markdown.slice(node.position?.start.offset, node.position?.end.offset).split("\n");
      const fence = /(`{3,}|~{3,})/.exec(source[0])?.[1];
      if (!fence) return;
      const closing = new RegExp(`^[\\s>]*${fence[0]}{${fence.length},}\\s*$`);
      if (source.length > 1 && closing.test(source.at(-1)!)) return;
      node.value = node.value!.replace(new RegExp(`(^|\\n)[ \\t]*${fence[0]}+[ \\t]*$`), "");
      node.data = { ...node.data, hProperties: { ...node.data?.hProperties, dataOpen: true } };
    };
    mark(tree);
  };
}

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

// A fenced code block, which scrolls sideways when wide, with a button that copies its code. Its
// syntax is colored once its closing fence arrives.
function CodeBlock({ node }: { node?: HastNode }) {
  const code = node!.children!.find((child) => child.tagName === "code")!;
  const text = code.children!.map((child) => child.value ?? "").join("");
  const open = code.properties?.dataOpen === true;
  const tag = (code.properties?.className as string[] | undefined)
    ?.find((name) => name.startsWith("language-"))
    ?.slice("language-".length);
  const lines = useColorRuns(text, open ? undefined : tag);
  return (
    <div className="code-block">
      <pre>
        <code>{lines ? lines.map((runs, index) => <ColoredLine key={index} runs={runs} last={index === lines.length - 1} />) : text}</code>
      </pre>
      <IconButton
        icon={Copy}
        label="Copy code"
        size="xs"
        className="code-block-copy"
        onClick={() => void navigator.clipboard.writeText(text)}
      />
    </div>
  );
}

function ColoredLine({ runs, last }: { runs: CodeRun[]; last: boolean }) {
  return (
    <>
      {runs.map((run, index) =>
        run.syntax ? (
          <span key={index} style={{ color: `var(--${run.syntax})` }}>
            {run.text}
          </span>
        ) : (
          <Fragment key={index}>{run.text}</Fragment>
        ),
      )}
      {!last && "\n"}
    </>
  );
}

// The colored runs of `code` in the language `tag` names, after the block first paints. Undefined
// until they are ready, with no tag, or for a language Monaco does not know.
function useColorRuns(code: string, tag: string | undefined): CodeRun[][] | undefined {
  const [colored, setColored] = useState<{ code: string; tag: string; lines?: CodeRun[][] }>();
  useEffect(() => {
    if (!tag) return;
    let current = true;
    colorRuns(code, tag).then(
      (lines) => current && setColored({ code, tag, lines }),
      (error) => console.error(`grove could not color a ${tag} block:`, error),
    );
    return () => {
      current = false;
    };
  }, [code, tag]);
  return colored?.code === code && colored.tag === tag ? colored.lines : undefined;
}

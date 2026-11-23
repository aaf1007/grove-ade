import { Copy, Ellipsis, Plus } from "lucide-react";
import { type ReactNode, useEffect, useState } from "react";

import type { SlashCommand, AgentState as State, Workspace } from "../bridge";
import { AgentMarkdown } from "../AgentMarkdown";
import { ChatPanel } from "../ChatPanel";
import { DetailsCard } from "../DetailsCard";
import type { FakeBridge, FakeWorktree } from "../fakeBridge";
import { PanelTabs } from "../PanelTabs";
import { Sidebar } from "../Sidebar";
import { AgentState } from "../ui/AgentState";
import { Button } from "../ui/Button";
import { Card } from "../ui/Card";
import { IconButton } from "../ui/IconButton";
import { Menu } from "../ui/Menu";
import { Segmented } from "../ui/Segmented";
import "./Gallery.css";

const MAIN = "/code/grove";
const FEATURE = "/code/grove-wt/feat-235";

/** The repo the gallery's fake bridge tracks. */
export const GALLERY_REPO: Record<string, FakeWorktree[]> = {
  [MAIN]: [
    { path: MAIN, branch: "main", isMain: true },
    { path: FEATURE, branch: "feat-235-ui-primitives", isMain: false },
  ],
};

/** An agent reply with every markdown element chat mode renders. */
const MARKDOWN_SAMPLE = [
  "## Parser fix",
  "",
  "The lexer **drops** the last token when the input has no trailing newline, so `next_token` returns *None* one token early.",
  "See [the logos docs](https://docs.rs/logos) for how spans end.",
  "",
  "1. Add a failing test",
  "2. Fix `next_token`",
  "   - keep the span",
  "   - run the gates",
  "",
  "> Diff parity is unaffected: this touches only the lexer.",
  "",
  "```rust",
  "fn next_token(&mut self) -> Option<Token> {",
  "    let token = self.tokens.pop()?; // a long line that scrolls sideways when the chat lane is narrower than the code is wide",
  "    Some(token)",
  "}",
  "```",
  "",
  "| Gate | Result |",
  "| --- | --- |",
  "| cargo clippy | pass |",
  "| vitest run | pass |",
].join("\n");

/** The slash commands and skills the gallery's Claude session lists. */
const GALLERY_COMMANDS: SlashCommand[] = [
  { name: "review", description: "Review a pull request", argumentHint: "<pr>", invocation: "/review" },
  { name: "init", description: "Write a CLAUDE.md for this codebase", argumentHint: "", invocation: "/init" },
  { name: "compact", description: "Summarize the conversation so far", argumentHint: "[instructions]", invocation: "/compact" },
  { name: "pr-comments", description: "Read the comments on a pull request", argumentHint: "<pr>", invocation: "/pr-comments" },
  { name: "security-review", description: "Review the branch for security issues", argumentHint: "", invocation: "/security-review" },
];

const STATES: State[] = [
  { state: "Working" },
  { state: "NeedsInput", kind: "Approval" },
  { state: "NeedsInput", kind: "Question" },
  { state: "Failed" },
  { state: "Done" },
  { state: "Idle" },
  { state: "Exited" },
];

const WORKSPACE: Workspace = {
  chat: [],
  panels: ["Files", "Diff", "Profile"],
  selected: "Files",
  terminal: false,
};

// What each gallery session's first message asks, one per state.
const TITLES = [
  "Fix the login redirect",
  "Add the diff parity test",
  "Check the parser edge cases",
  "Run the gates",
  "Review the sidebar",
  "Rename the profile field",
  "Spike the port router",
];

/**
 * Starts one agent session per state on the fake bridge, so the sidebar shows each: the Question
 * one as a subagent of the first, the Idle one in the local checkout, and the Exited one ended.
 */
export async function addGallerySessions(bridge: FakeBridge) {
  let first: string | undefined;
  for (const [at, state] of STATES.entries()) {
    const worktree = state.state === "Idle" ? MAIN : FEATURE;
    const parent = state.state === "NeedsInput" && state.kind === "Question" ? first : undefined;
    const session = bridge.spawnAgentWithoutPanel(worktree, at % 3 === 2 ? "Codex" : "Claude", parent);
    first ??= session;
    await bridge.sendMessage(session, TITLES[at]);
    bridge.agentStateChanged({ session, ...state, source: "Protocol" });
    if (state.state === "Exited") bridge.exit(session);
  }
}

/**
 * Every UI component in every state, on the fake bridge, with a light and dark switch. The dev
 * server serves it at /gallery.html.
 */
export function Gallery({ bridge }: { bridge: FakeBridge }) {
  const [theme, setTheme] = useState<"light" | "dark">("dark");
  const [mode, setMode] = useState<"Chat" | "Terminal">("Chat");
  const [workspace, setWorkspace] = useState(WORKSPACE);
  const [pane, setPane] = useState<string | null>(null);
  const [codex, setCodex] = useState<string | null>(null);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
  }, [theme]);

  // A real agent session on the fake bridge, for the pane head.
  useEffect(() => {
    let current = true;
    bridge.spawnAgent(FEATURE, "Claude", null).then((session) => {
      if (!current) return;
      bridge.agentStateChanged({ session, state: "Working", source: "Protocol" });
      // A / in its composer opens the command menu.
      bridge.messages(session, { type: "Commands", commands: GALLERY_COMMANDS });
      setPane(session);
    });
    return () => {
      current = false;
    };
  }, [bridge]);

  // The readings the details cards show: Claude's plan windows and context, and for Codex a last
  // good reading from before a failed read, with no context window.
  useEffect(() => {
    if (!pane) return;
    let current = true;
    const now = Date.now();
    const minutes = 60_000;
    const windows = (fiveHour: number, weekly: number) => ({
      kind: "Windows" as const,
      windows: [
        { name: "5-hour", percentUsed: fiveHour, resetsAtUnixMs: now + 135 * minutes },
        { name: "Weekly", percentUsed: weekly, resetsAtUnixMs: now + 4 * 24 * 60 * minutes },
      ],
    });
    bridge.usageReadingChanged({
      agent: "Claude",
      reading: { limits: windows(42, 18), readAtUnixMs: now, latestReadFailed: false },
    });
    bridge.usageReadingChanged({
      agent: "Codex",
      reading: { limits: windows(71, 36), readAtUnixMs: now - 12 * minutes, latestReadFailed: true },
    });
    bridge.contextReadingChanged({
      session: pane,
      reading: { sessionTotalTokens: 1_240_000, contextWindow: { usedTokens: 84_000, sizeTokens: 200_000 } },
    });
    bridge.spawnAgent(FEATURE, "Codex", null).then((session) => {
      if (!current) return;
      bridge.agentStateChanged({ session, state: "Done", source: "Protocol" });
      bridge.contextReadingChanged({ session, reading: { sessionTotalTokens: 30_500, contextWindow: null } });
      setCodex(session);
    });
    return () => {
      current = false;
    };
  }, [bridge, pane]);

  return (
    <main className="gallery">
      <header className="gallery-head">
        <h1>Components</h1>
        <Segmented<"light" | "dark">
          label="Theme"
          options={[
            ["light", "Light"],
            ["dark", "Dark"],
          ]}
          value={theme}
          onChange={setTheme}
        />
      </header>

      <Section title="Button">
        <Row>
          <Button variant="primary">Add repo</Button>
          <Button>Review diff</Button>
          <Button variant="ghost">Cancel</Button>
          <Button disabled>Send review batch</Button>
        </Row>
        <Row>
          <Button size="sm" icon={Plus}>
            New worktree
          </Button>
          <Button size="sm" variant="primary">
            Allow once
          </Button>
          <Button size="sm" variant="ghost">
            Deny
          </Button>
        </Row>
        <Row>
          <Button size="xs">Open diff</Button>
          <Button size="xs" variant="primary">
            Allow
          </Button>
          <Button size="xs" variant="ghost" icon={Plus}>
            Start Claude Code
          </Button>
          <Button size="xs" disabled>
            Save
          </Button>
        </Row>
      </Section>

      <Section title="IconButton">
        <Row>
          <IconButton icon={Copy} label="Copy" />
          <IconButton icon={Plus} label="Pressed" aria-pressed={true} />
          <IconButton icon={Ellipsis} label="Expanded" aria-expanded={true} />
          <IconButton icon={Copy} label="Disabled" disabled />
          <IconButton icon={Plus} label="Small" size="xs" />
        </Row>
      </Section>

      <Section title="AgentState">
        <Row>
          {STATES.map((state) => (
            <AgentState key={JSON.stringify(state)} state={state} />
          ))}
        </Row>
      </Section>

      <Section title="Segmented">
        <Row>
          <Segmented<"Chat" | "Terminal">
            label="Mode"
            options={[
              ["Chat", "Chat"],
              ["Terminal", "Terminal"],
            ]}
            value={mode}
            onChange={setMode}
          />
          <Segmented
            label="Diff base"
            options={[
              ["head", "HEAD"],
              ["merge", "Merge base"],
            ]}
            value="merge"
            onChange={() => {}}
          />
          <Segmented
            label="Mode while working"
            options={[
              ["Chat", "Chat"],
              ["Terminal", "Terminal"],
            ]}
            value="Chat"
            disabled
            title="Claude Code is working. Switch modes once the turn ends."
            onChange={() => {}}
          />
        </Row>
      </Section>

      <Section title="PanelTabs">
        <div className="gallery-frame">
          <PanelTabs
            workspace={workspace}
            onSelect={(selected) => setWorkspace((w) => ({ ...w, selected }))}
            onClose={(tab) =>
              setWorkspace((w) => {
                const panels = w.panels.filter((open) => open !== tab);
                return { ...w, panels, selected: w.selected === tab ? (panels.at(-1) ?? null) : w.selected };
              })
            }
            onOpen={(panel) =>
              setWorkspace((w) =>
                panel === "Terminal"
                  ? { ...w, terminal: !w.terminal }
                  : { ...w, panels: w.panels.includes(panel) ? w.panels : [...w.panels, panel], selected: panel },
              )
            }
            onToggleTerminal={() => setWorkspace((w) => ({ ...w, terminal: !w.terminal }))}
            onTogglePanels={() => {}}
          />
        </div>
        <div className="gallery-frame">
          <PanelTabs
            workspace={{ ...WORKSPACE, panels: [], selected: null }}
            onSelect={() => {}}
            onClose={() => {}}
            onOpen={() => {}}
            onToggleTerminal={() => {}}
            onTogglePanels={() => {}}
          />
        </div>
        <div className="gallery-frame">
          <PanelTabs
            workspace={null}
            onSelect={() => {}}
            onClose={() => {}}
            onOpen={() => {}}
            onToggleTerminal={() => {}}
            onTogglePanels={() => {}}
          />
        </div>
      </Section>

      <Section title="Menu">
        <Row>
          <Menu
            label="Actions for feat-235"
            icon={Ellipsis}
            items={[
              { key: "base", label: "Change base branch", onSelect() {} },
              { key: "forget", label: "Forget", onSelect() {} },
              { key: "delete", label: "Delete", onSelect() {} },
            ]}
          />
          <Menu
            label="Open a panel"
            icon={Plus}
            heading="Open a panel"
            items={[
              { key: "Files", label: "Files", checked: true, shortcut: "F", onSelect() {} },
              { key: "Terminal", label: "Terminal", hint: "Opens at the bottom", checked: false, shortcut: "T", onSelect() {} },
            ]}
          />
        </Row>
      </Section>

      <Section title="Card">
        <Card
          title="3 files changed"
          actions={
            <Button size="xs" onClick={() => {}}>
              Review diff
            </Button>
          }
        >
          app/src/ui/Button.tsx
        </Card>
        <Card title="A card with no body" />
      </Section>

      <Section title="AgentMarkdown">
        <div className="gallery-chat-lane">
          <AgentMarkdown text={MARKDOWN_SAMPLE} onOpenLink={() => {}} />
        </div>
      </Section>

      <Section title="DetailsCard">
        <Row>
          {[
            { session: pane, agent: "Claude" as const, state: "Working" as const },
            { session: codex, agent: "Codex" as const, state: "Done" as const },
          ].map(
            ({ session, agent, state }) =>
              session && (
                <div key={agent} className="gallery-details">
                  <DetailsCard
                    bridge={bridge}
                    worktree={FEATURE}
                    runs={[]}
                    session={{
                      session,
                      agent,
                      model: null,
                      mode: "Chat",
                      state: { session, state, source: "Protocol" },
                    }}
                    onStartRun={() => {}}
                    onOpenDiff={() => {}}
                    onOpenProfile={() => {}}
                  />
                </div>
              ),
          )}
        </Row>
      </Section>

      <Section title="On the fake bridge">
        <div className="gallery-in-app">
          <Sidebar
            bridge={bridge}
            selected={FEATURE}
            onSelect={() => {}}
            agentStates={{}}
            shownAgents={pane ? [pane] : []}
            onStartAgent={() => {}}
            onStartAgentAfterSetup={() => {}}
            onNewSession={() => {}}
            onFocusAgent={() => {}}
            onSessionResumed={() => {}}
            onSessionsDeleted={() => {}}
            onHideSidebar={() => {}}
            onDeleting={() => {}}
            onRemoved={() => {}}
            waiting={1}
            onJump={() => {}}
            onOpenProfiles={() => {}}
            accounts={{ Claude: { email: "dev@example.com", organization: null, plan: "Max", apiKeySource: null } }}
            appearance={{ theme: "system", palette: "default", appIcon: "auto" }}
            onAppearance={() => {}}
          />
          {pane && (
            <ChatPanel
              bridge={bridge}
              session={pane}
              agent="Claude"
              sessionName="Claude Code 2"
              worktree={FEATURE}
              shown
              state={{ session: pane, state: "Working", source: "Protocol" }}
              focusRequest={0}
              onOpenDiff={() => {}}
              onSplit={() => {}}
              onHide={() => {}}
            />
          )}
        </div>
      </Section>
    </main>
  );
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="gallery-section" aria-label={title}>
      <h2>{title}</h2>
      {children}
    </section>
  );
}

function Row({ children }: { children: ReactNode }) {
  return <div className="gallery-row">{children}</div>;
}

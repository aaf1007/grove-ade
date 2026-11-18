// Runs the real sidecar and the real SDK against a fake claude that records its arguments.
import { spawn } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

const MAIN = join(import.meta.dirname, "main.ts");
const SESSION_ID = "0b0f6a52-3f5e-4c2a-9d38-6a8c1f2e7d10";
const SESSION_TOKEN = "grove-session-token-5b1e0c";

// Writes its arguments to $FAKE_CLAUDE_ARGS and exits, which ends the sidecar.
const FAKE_CLAUDE = `#!/usr/bin/env node
require("node:fs").writeFileSync(process.env.FAKE_CLAUDE_ARGS, JSON.stringify(process.argv.slice(2)));
`;

const dirs: string[] = [];
afterEach(() => {
  for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

/** Claude's record of a conversation with one turn, where the SDK looks for it. */
function writeRecord(config: string, cwd: string) {
  const project = join(config, "projects", cwd.replace(/[^a-zA-Z0-9]/g, "-"));
  mkdirSync(project, { recursive: true });
  const line = (type: string, uuid: string, parentUuid: string | null, content: unknown) =>
    JSON.stringify({ type, uuid, parentUuid, sessionId: SESSION_ID, cwd, isSidechain: false, timestamp: "2026-11-14T00:00:00Z", message: { role: type, content } });
  const user = line("user", "5c7e1a40-0000-4000-8000-000000000001", null, "fix the parser");
  const reply = line("assistant", "5c7e1a40-0000-4000-8000-000000000002", "5c7e1a40-0000-4000-8000-000000000001", [{ type: "text", text: "Done." }]);
  writeFileSync(join(project, `${SESSION_ID}.jsonl`), `${user}\n${reply}\n`);
}

/**
 * The arguments the sidecar launches claude with, for a session whose mcp.json holds the token.
 * `record`: Claude has written the conversation, as it does after the first message.
 */
async function claudeArgs(resume: boolean, record = false): Promise<{ args: string[]; mcpConfig: string }> {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), "grove-sidecar-")));
  dirs.push(dir);
  const config = join(dir, "config");
  if (record) writeRecord(config, dir);
  const claude = join(dir, "claude");
  writeFileSync(claude, FAKE_CLAUDE);
  chmodSync(claude, 0o755);
  const settings = join(dir, "claude-settings.json");
  writeFileSync(settings, JSON.stringify({ hooks: {} }));
  const mcpConfig = join(dir, "mcp.json");
  const grove = { command: "grove-mcp", args: [], env: { GROVE_SESSION_ID: SESSION_ID, GROVE_SESSION_TOKEN: SESSION_TOKEN } };
  writeFileSync(mcpConfig, JSON.stringify({ mcpServers: { grove } }));
  const argsFile = join(dir, "args.json");

  const flags = ["--settings", settings, "--mcp-config", mcpConfig, "--session-id", SESSION_ID, "--claude", claude];
  const sidecar = spawn(process.execPath, [MAIN, ...flags, ...(resume ? ["--resume"] : [])], {
    cwd: dir,
    // A config folder of its own, so a resume never reads the user's conversations.
    env: { ...process.env, FAKE_CLAUDE_ARGS: argsFile, CLAUDE_CONFIG_DIR: config },
    stdio: ["pipe", "ignore", "ignore"],
  });
  // The core keeps stdin open; the sidecar ends when the fake claude exits.
  await new Promise((resolve) => sidecar.on("exit", resolve));
  return { args: JSON.parse(readFileSync(argsFile, "utf8")), mcpConfig };
}

describe("the claude the sidecar launches", () => {
  for (const resume of [false, true]) {
    it(`never has the session token in its arguments, ${resume ? "on resume" : "on launch"}`, async () => {
      const { args, mcpConfig } = await claudeArgs(resume, resume);

      expect(args.filter((arg) => arg.includes(SESSION_TOKEN))).toEqual([]);
      expect(args).toContain(`${resume ? "--resume" : "--session-id"}=${SESSION_ID}`);
      expect(args[args.indexOf("--mcp-config") + 1]).toBe(mcpConfig);
      // Two node processes start: slow when other builds load the machine.
    }, 30_000);
  }

  it("starts a new conversation under the same id when --resume finds no record", async () => {
    const { args } = await claudeArgs(true);

    expect(args).toContain(`--session-id=${SESSION_ID}`);
    expect(args.some((arg) => arg.startsWith("--resume"))).toBe(false);
  }, 30_000);
});

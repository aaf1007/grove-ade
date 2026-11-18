// The sidecar's entry point: wires run() to stdio, the session's files and the user's claude.
import { accessSync, constants } from "node:fs";
import { delimiter, join, resolve } from "node:path";
import { createInterface } from "node:readline";

import { getSessionMessages, query } from "@anthropic-ai/claude-agent-sdk";

import { parseArgs, run, Usage } from "./sidecar.ts";

/** `claude` as a path: a bare name is looked up on PATH, as a shell would. */
function findClaude(claude: string): string {
  if (claude.includes("/")) return resolve(claude);
  for (const dir of (process.env.PATH ?? "").split(delimiter)) {
    const path = join(dir, claude);
    try {
      accessSync(path, constants.X_OK);
      return path;
    } catch {
      // Not in this folder.
    }
  }
  throw new Usage(`${claude} is not on PATH`);
}

function main() {
  const launch = parseArgs(process.argv.slice(2));
  const claude = findClaude(launch.claude);
  return run(
    { ...launch, claude },
    {
      query,
      lines: createInterface({ input: process.stdin, crlfDelay: Infinity }),
      write: (message) => process.stdout.write(`${JSON.stringify(message)}\n`),
      cwd: process.cwd(),
      sessionMessages: (sessionId, dir) => getSessionMessages(sessionId, { dir }),
    },
  );
}

try {
  await main();
  process.exit(0);
} catch (error) {
  console.error(error instanceof Usage ? `claude sidecar: ${error.message}` : error);
  process.exit(error instanceof Usage ? 2 : 1);
}

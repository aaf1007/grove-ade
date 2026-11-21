import { expect, test, vi } from "vitest";

import { fakeBridge } from "./fakeBridge";

const main = "/code/grove";

test("the fake tells session-list listeners when an agent session starts, gets its title, switches mode or ends", async () => {
  const bridge = fakeBridge({ [main]: [{ path: main, branch: "main", isMain: true }] });
  await bridge.addRepo(main);
  const changed = vi.fn();
  bridge.onAgentSessionsChanged(changed);

  const session = await bridge.spawnAgent(main, "Claude", null);
  expect(changed).toHaveBeenCalledTimes(1);
  await bridge.sendMessage(session, "Fix the login bug");
  expect(changed).toHaveBeenCalledTimes(2);
  await bridge.setMode(session, "Terminal");
  expect(changed).toHaveBeenCalledTimes(3);
  await bridge.kill(session);
  expect(changed).toHaveBeenCalledTimes(4);
  expect((await bridge.agentSessions())[0]).toMatchObject({ mode: "Terminal", ended: true, title: "Fix the login bug" });
});

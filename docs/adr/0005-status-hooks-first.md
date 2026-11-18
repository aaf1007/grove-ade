# 0005: Agent status from hooks first, heuristics last

- Status: Superseded in part by 0010 (integration install and the Notify source) and 0014 (the state source in chat mode)
- Context: Some tools detect status from process names and output heuristics, which can be wrong. Claude Code has hooks; Codex has `notify`.
- Decision: Priority Hook > Notify > Heuristic. `grove-hook` relays payloads tagged with `GROVE_SESSION_ID`.
- Consequences: Each adapter installs its integration into the worktree. Heuristics still needed for Codex approval prompts unless its version has hooks.
- Revisit if: either agent ships an official status API.

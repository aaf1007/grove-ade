# 0016: Profiles, and agents that shape the workspace

- Status: Accepted (Nov 2026)
- Context: Developers work differently across projects, languages and frameworks. grove's headline is now that it fits each workflow: the user, or an agent, can change it per project. Repo settings today hold only run sessions and launch args, in grove's own state.
- Decision:
  - A profile is a TOML file in `~/.grovecode/profiles/`. It holds the default agent, model, effort and permission mode, the workspace layout, setup, cleanup and run sessions, launch args, start prompts, the review flow and which agent states notify.
  - A repo picks one profile and may override parts of it. grove suggests a profile from marker files such as `Cargo.toml` or `next.config.js`; the user picks.
  - Agents read and change the profile and the workspace layout through `grove-mcp`, using only grove's built-in panels. Every change an agent makes shows a notice with Undo, Keep and Save to profile.
  - After M7: agent-written widgets (sandboxed HTML panels with a grove API), and an optional profile committed in the repo.
- Consequences: Repo settings shrink to the chosen profile plus overrides. The core needs a TOML parser, a new dependency to approve when the first profile issue lands. Profiles are plain files, so they can be read, diffed and shared like code.
- Revisit if: profiles turn out too coarse during dogfooding, or agent-written widgets are needed before M7.

`mudpuppy install claude`: writes Claude Code skills to disk so a separate agent learns the mudpuppy review loop and surfaces it as an option. No AI is added to the binary — this only writes files. Scope is project (committed), local (git-ignored via `.git/info/exclude`), or user (`~/.claude/skills/`); prompts interactively when `--location` is omitted.

- `mod.rs`: `Location` enum, scope resolution, idempotent skill-file writes (version-stamped into the frontmatter), overwrite/scope prompts, local `.git/info/exclude` updates, plus staleness detection (`installed_skill_status`/`should_prompt_skill_refresh`), in-place `refresh_installed_skills`, and the `skills-skipped-version` skip file.

When you change a skill body, bump `SKILL_VERSION` and rewrite `SKILL_UPDATE_MESSAGE` (in `mod.rs`) to say what changed, so installed copies are flagged stale and the refresh prompt describes the update.
- `pr_review.md`: embedded `SKILL.md` body teaching the PR-review workflow (read-only `gh`, annotate via mudpuppy, turn loop).
- `implementation_review.md`: embedded `SKILL.md` body teaching the agent to flag its own local changes for review and read feedback.

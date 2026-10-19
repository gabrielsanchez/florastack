# CLAUDE.md

@AGENTS.md

## Notes for Claude Code

AGENTS.md, imported above, holds the project's rules. These notes add what is specific to working as Claude Code.

- Every session starts fresh. Read `docs/ROADMAP.md` and the recent `git log` to see where the build stands before you change anything.
- For a roadmap step marked *(plan first)*, present a plan and wait for it to be approved before you edit files.
- Build each roadmap step on a branch of its own, and commit only when asked, once its acceptance checks pass.
- Finish with `cargo xtask ci` and report its summary as it is: the step that failed, the tests that were skipped, and any check this machine couldn't run. Never tick a roadmap item whose checks haven't passed.
- The clean-room rule covers what you read: open reference code only from permissively licensed projects (PhotoCraft, LightCraft, focus-stack), never from the GPL and LGPL projects that AGENTS.md names.
- Keep scratch files and downloads out of the repository, and machine-specific settings out of committed files, this one included.
- Never put personal data, such as a name or an email address, in a web request: use a generic User-Agent.

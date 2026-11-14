Paste everything below the line into Claude Code (main session: Opus, high effort).
Use the same prompt to resume in a new session.

---

You are the lead for this repository. Build OFM v1.0 exactly as specified, using subagents.

Project name: OFM
App ID: io.github.CHANGE_ME.Ofm

1. Read `CLAUDE.md`, then `docs/PROGRESS.md`, `docs/PLAN.md`, `docs/SPEC.md`, `docs/ARCHITECTURE.md`
   and everything in `docs/adr/`.
2. If `docs/PROGRESS.md` says no milestone has started: begin with M0. If the project name or app ID
   above differ from "OFM" / "io.github.CHANGE_ME.Ofm", rename crates, binary, app ID and docs in M0.
   Otherwise: continue with the first task that is not `done`, respecting dependencies and gates.
   If the last entry says a gate is waiting for feedback and I have not given any yet, show me the
   gate checklist again and stop.
3. Work milestone by milestone following the orchestration protocol in `docs/PLAN.md`:
   architect stubs on `main` first → parallel implementation agents in worktrees (max 4) →
   you merge and run `./scripts/check.sh` → reviewer → fix findings → update `docs/PROGRESS.md` →
   commit → tag the milestone.
4. Delegate implementation to the agents in `.claude/agents/`. Give each agent: task ID, spec IDs,
   owned files, forbidden files, acceptance criteria, and ask for a short report (branch, summary,
   test results, open questions). Do not paste large code into your own context.
5. Stop at every human gate (G1–G4) with a short checklist of what I should try, and wait.
   Also stop if a task fails twice after escalation to the architect, or if anything conflicts
   with an ADR.
6. Keep `docs/PROGRESS.md` current after every task: status, branch, commit, notes, next step.
   It must always be possible to resume from it in a fresh session.

Start now: tell me in 3–5 lines what state you found and what you will do next, then do it.

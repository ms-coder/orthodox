# OFM starter kit — build v1.0 with Claude Code

This folder holds everything Claude Code needs to build OFM v1.0 on its own, milestone by milestone,
using subagents. You drop it into an empty repository, run one prompt, and come back at the review gates.

## What's inside

| File | Purpose |
|---|---|
| `CLAUDE.md` | Project rules Claude Code loads in every session (stack, invariants, git flow, gates) |
| `KICKOFF.md` | The prompt you paste into Claude Code to start (and to resume) |
| `docs/SPEC.md` | v1.0 feature spec: F01–F33, themes, quality bar, out-of-scope list |
| `docs/ARCHITECTURE.md` | Crates, core types and traits, threading model, data-safety design |
| `docs/PLAN.md` | Milestones M0–M10, ~50 tasks, agent per task, parallel groups, acceptance, human gates |
| `docs/PROGRESS.md` | Progress tracker the lead session keeps current — this is what makes it resumable |
| `docs/adr/0001-stack.md` | First architecture decision record |
| `.claude/agents/*.md` | 7 subagents: architect, core, fileops, ui, test, reviewer, release |
| `.claude/settings.json` | Lets Claude Code run cargo/git/scripts without asking each time |
| `scripts/bootstrap-ubuntu.sh` | Installs the system packages (GTK 4, libadwaita, VTE, Xvfb, Rust) |
| `scripts/check.sh` | The one "is everything green?" command |
| `scripts/screenshot.sh` | Runs the app under Xvfb and saves a PNG, so agents can look at the UI |
| `scripts/make-test-dirs.sh` | Creates test folders for you (100k files, Cyrillic encodings, symlinks, sparse file, archives) |

## How to start

```bash
mkdir ~/dev/ofm && cd ~/dev/ofm
unzip ~/Downloads/ofm-starter-kit.zip -d .        # or copy the folder contents here
./scripts/bootstrap-ubuntu.sh                     # once; needs sudo for apt
git init -b main && git add -A && git commit -m "chore: starter kit"
claude                                            # start Claude Code in this folder
```

Then:

1. Choose the model for the main session (see below): `/model` → Opus, `/effort` → high.
2. Paste the contents of `KICKOFF.md`.
3. Approve the first few permission prompts (or use auto-accept for edits). Agents run in the background.
4. Come back at each **gate** (G1–G4): Claude Code stops, gives you a short "try this" list, and waits.

To resume later (new session, after a break, after `/clear`): start `claude` in the folder and paste
`KICKOFF.md` again. It tells Claude to continue from `docs/PROGRESS.md`.

## Which model: Sonnet medium or high?

Short answer: **don't run the whole project on Sonnet medium.** Use a mix. It's already configured in
the agent files:

| Role | Model / effort | Why |
|---|---|---|
| Main session (lead) | **Opus, high** | Plans, merges branches, fixes integration bugs across crates — the hardest, longest-horizon job. A bad merge or a wrong design call costs more than the tokens saved. |
| `architect`, `reviewer`, `fileops-engineer` | **Opus, high** | Interfaces, review, and the copy/move engine where a bug means lost files. |
| `core-engineer`, `ui-engineer` | **Sonnet, high** | Bulk of the code. Rust + GTK 4 (GObject subclassing, `ListModel`, lifetimes across threads) produces many compile-error loops; high effort gets through them in fewer iterations, which usually costs about the same as medium in the end. |
| `test-engineer`, `release-engineer` | **Sonnet, medium** | Well-specified, routine work. |

**If you hit usage limits:** run the main session on Sonnet high instead of Opus, and keep Opus only for
`architect`, `fileops-engineer`, and `reviewer` — those three are where quality matters most.

**Sonnet medium everywhere** would work for simple tasks but tends to stall on GTK 4 subclassing and on
the threading between workers and the UI; you'd spend your attention fixing what it got wrong.

To change an agent's model or effort, edit the `model:` / `effort:` line in `.claude/agents/<name>.md`.

## What to expect honestly

- This is a multi-session project. Plan on roughly 10 milestones; each typically takes one long session
  or several shorter ones. `docs/PROGRESS.md` carries the state between sessions.
- You are the product owner at the gates: G1 (UI shell + themes), G2 (file operations), G3 (all features),
  G4 (release candidate). Your feedback there matters most; between gates it runs on its own.
- The plan is designed so the risky part (copy/move engine) is tested heavily and reviewed by a separate
  agent before you rely on it. Still: **test on a copy of real data first.**

## Naming

The working name is **OFM** (crates `ofm-*`, binary `ofm`, app ID `io.github.CHANGE_ME.Ofm`).
To pick your own name, edit the two lines in `KICKOFF.md` before you paste it; the lead renames
everything in milestone M0.

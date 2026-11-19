## Devlog (MCP) — always on, every session

Devlog is my todo list, time tracking, docs and memory, and it is the first thing you touch in every
session. Work out of it instead of asking me what to do.

Kairo lives in the **Kairo** list (`list_d9e5eede18c0`). Documents use `kairo-*` slugs, memories use
`kairo.*` keys, both tagged `kairo`. The 206 completed tasks are a snapshot of the trackers, so new
work goes in as tasks first and the trackers follow.

**Session start, before anything else**
- `get_workspace` with `view=today`, then `pull_next_todo`.
- `memory_recall` by key (`kairo.status`, `kairo.stack`) for project facts.
- `doc_list` / `doc_get` before asking me about the project; the docs are the source of truth.
- There is no `daily_briefing` on this Worker; those three calls replace it.

**While working**
- `start_task` with a `taskId` (or exact title) before editing files. No match? `add_task` in the
  Kairo list and start it. `quick_add` does not exist.
- Split big work into subtasks (`add_tasks_batch`, `toggle_subtask`) and log blockers, findings and
  decisions with `update_task` notes.
- Found a new bug or idea? `add_task` immediately.

**When done**
- `complete_task` with `pullNext=true`, then tell me what is next.
- Stopping mid-task: `stop_task` with a `note`.
- Time review: `get_time_insights`. There is no `end_of_day_review` or `undo_last`.

**Memory and docs**
- `memory_save` for decisions, conventions and gotchas: short, keyed (`kairo.config`), reuse the key
  to update.
- `doc_upsert` reusing the `kairo-*` slug; never create a second doc for the same topic.
- `memory_forget` only with explicit confirmation.

**Devlog rules**
- Never guess a taskId. If a title match is ambiguous, show the candidates.
- Only one timer runs at a time. Do not start a task I have not asked for.
- Never run `delete_task`, `delete_list`, `memory_forget` or any destructive call unless I ask.
- If a tool named here is missing, say so and use the closest real one from the server's 23 tools.
- Do not write "Devlog" or reference-project names in code comments or commit messages.


# Kairo: agent instructions

Follow docs/MAIN_RULES.md and docs/LEAN_RULES.md for every feature. The reference-port skill also points
at them: read both before porting any behaviour.

A Lavalink v4 compatible server in pure Rust. Read this file first, every session.

## Read order
1. `docs/PROJECT.md`   everything about the project (scope, design, decisions, risks)
2. `docs/PROGRESS.md`  what is done, in progress, next
3. `docs/CODING_RULES.md`  how code must look
4. `docs/DEVIATIONS.md`  where we differ from the reference on purpose (create it when first needed)

## Scope right now
Only: voice, player (with filters), server, and native provider sources beginning with JioSaavn.
Not now: runtime plugins, YouTube, other providers (Spotify, Apple, Deezer, ...), lyrics, SponsorBlock or queue.
Do not build them. Do not design them out.

## Reference code
`./reference` (inside this repo) is read-only. Never edit it, never commit it (gitignored).
Never mention any reference project in code, comments or commit messages.

## Workflow (one feature at a time)
1. Pick the top item with `pull_next_todo` from Devlog, then `start_task` on it. That queue mirrors
   "Next" in `docs/PROGRESS.md`; if the two disagree, fix the Devlog task before starting.
2. Read the matching reference files. Understand behaviour first, then write Rust.
3. Implement it small and clean.
4. `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
5. Update `docs/PROGRESS.md` (and `docs/PROJECT.md` if a decision or design changed), then
   `complete_task` with `pullNext=true`.
6. One small commit that contains the code and the PROGRESS update together.

A feature is not done until step 5 is in the same commit.

## Skills (use them, they are in .claude/skills)
- `rust-human-code`       how to write clean, human-looking, professional Rust. Use for every code change.
- `rust-realtime-audio`   hot-path rules for decode, pacing, buffers, backpressure. Use for voice and player work.
- `feature-workflow`      the loop above, with the PROGRESS and commit rules.
- `reference-port`        how to study the Java reference and port behaviour without copying structure.

## Plugins and skills policy
Before starting a task, check which installed plugins and skills apply (for example a Rust language server
plugin) and use them. If a recurring need has no skill, create one in `.claude/skills/<name>/SKILL.md`
(frontmatter with `name` and `description`), then list it here. Keep each skill short and specific.

## Hard rules
- No `unwrap`/`expect` outside tests. No `unsafe` unless justified in a one-line comment and PROJECT.md.
- No blocking calls on the async runtime. No unbounded channels or buffers.
- Every long-lived task has a shutdown path.
- No C dependencies in the audio or voice path.
- Never hardcode keys, tokens or cookies.
- Use at most 4-6 sub-agents at a time.
- If something in the reference or a library is unclear, say so and ask. Do not guess protocol details.

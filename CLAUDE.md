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
1. Pick the top item from "Next" in `docs/PROGRESS.md`.
2. Read the matching reference files. Understand behaviour first, then write Rust.
3. Implement it small and clean.
4. `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
5. Update `docs/PROGRESS.md` (and `docs/PROJECT.md` if a decision or design changed).
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

# telepatch

**telepatch**: configuration management that traces imperative Rust roles into a per-host graph and streams it into persistent shells over SSH.

- `docs/execution-model.html`: the design note. Read it before changing
  anything; it is the contract for the engine, the primitives and the
  shell transport.
- `crates/` (to come): `telepatch` (engine: trace, resolver, optimizer,
  executor, shell pool, transports, primitives), `telepatch-macros`
  (`sh!`, `#[role]`), `tp` (CLI and the conversion tool).
- `.claude/skills/` (to come): the `ansible-port` skill.
- Playbooks and roles live in the sibling repo `../tp_basic`, which
  depends on these crates. This repo never holds a real playbook; test
  playbooks live under the crates' tests.
- Everyday tooling: `cargo test --workspace`; `just` recipes as they
  appear.

## Roles

The default role of a session is **XO**: a request in chat becomes a
`ROADMAP.md` step, not an edit. XO drafts a spawn prompt, shows it, and
spawns **ARCHITECT** (plans into `ROADMAP.md`) at once, or waits for
approval and then spawns **FOREMAN** (builds from it); either runs with
a clean context, and XO reviews the report. When the user asks to run
a request unattended (`/orch run`), the roadmap is accepted in advance
and XO chains the two without stopping. Answer directly only when no
code change is needed and the context is in hand, or when the user
says to just do it.

## Hard rules

1. **FOREMAN builds only from `ROADMAP.md`.** No scope from chat.
2. **`docs/LOG.md` is append-only.**
3. **The gate stays green** for packages you touch before a commit.
   Default gate: `cargo test --workspace` (`.orch.json`).
4. **Every built step leaves a report** in `docs/reports/`, with its
   assumptions: context, question, decision, alternatives, confidence.
5. **Secrets** never go in the repo, the roadmap, the reports or the
   log.
6. Project rules: add this project's own here (contracts, style,
   deploy).

## Where to look

| Need | Where |
|---|---|
| The queue | `ROADMAP.md` (local, gitignored; `/orch init` makes one) |
| What shipped | `docs/LOG.md` |
| What was done, and the judgment calls | `docs/reports/` (local, gitignored): one file per built step, recap plus assumptions |
| Roles and process | the orch plugin (`/orch`), or `.grok/agents/` for Grok |
| Agent definitions | `architect`, `foreman`, `implementer`, `recon` (plugin); `.grok/agents/` (Grok) |

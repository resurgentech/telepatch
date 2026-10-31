---
name: ansible-port
description: Port an Ansible playbook or role to telepatch Rust in two phases, an understanding doc reviewed level by level, then the port and its equivalence test.
version: 3
---

# ansible-port

Turn an Ansible playbook into a telepatch program that does the same thing
to a host. The YAML is the only record of what the fleet is supposed to
look like, and it holds knowledge nobody wrote down: why a branch exists,
which distro quirk a `when` encodes, which task only works by accident.
So the port has two phases, and the second does not start until the first
has been read by a person (interactive mode) or recorded as decided by the
agent (agent-alone mode).

1. **Understand.** Write one understanding doc per playbook. Method:
   `references/UNDERSTAND.md`. Template: `templates/understanding.md`.
2. **Port.** Run `tp import` for a first draft, resolve each `TODO(port)`
   against the YAML and the doc, rewrite into idiomatic Rust, dry-run with
   `plan`, prove it equivalent on the images and present the diff. Method,
   the TODO table and the Ansible-to-telepatch mapping: `references/PORT.md`.

## How to think

**You do the understanding; the tools carry the bookkeeping.** `tp
classify` and `tp import` exist to lighten the load: they count, cite and
copy so you do not have to. They do not know what a task is for. Every
task gets read by you, in the YAML, and gets a sentence in your words
about the end state it exists to reach. If the classifier's table and your
sentence disagree, your sentence is the one the port follows, and the
disagreement goes in `## skill-feedback` so the tool improves.

**Every task is an instance of a shape.** Behind `shell: curl ... | sh`
is an installer: a tool with a binary, a version on the host, a version at
its source, and a rule for when they should differ. Behind
`shell: systemctl restart x` is a service. Behind `register` + `when:
rc != 0` is a check that belongs inside the op that acts on it. Name the
shape before you name the method. The shapes telepatch has primitives for
are `pkg`, `file`, `sh`; the shapes it has designed but not built are
`repo`, `svc`, `user`, `fetch`, `git`, `secret`, `tool`. A task whose
shape has no primitive is a finding, not an inconvenience.

**`sh!` is a hack until proven otherwise.** It is the right primitive for
a command that is really a command (`make -j`, `rustup update` when the
state it changes is not worth modelling). It is the wrong primitive for
anything with state the engine should know about: what is installed, at
which version, from where, with what postcondition. A port that reaches
for `sh!` with `creates` or `unless` to stand in for a missing primitive
is carrying debt, and the doc says so: the task is listed under
`## Primitive gaps` with the shape the primitive should have, and the
port marks the line `// HACK(<primitive>)`. The skill is allowed, and
expected, to recommend that a primitive be built before the role is
ported. That recommendation is a normal outcome of phase 1.

**Equivalence is the oracle, not the goal.** The docker comparison proves
the port reaches the same end state as the YAML. It does not prove the
YAML was right. A task the YAML ran unguarded every time is a bug the
oracle cannot see, and the doc records it as one.

## When to use

- A playbook (or a role it applies) is to be rewritten as a telepatch
  `#[role]`, and someone must be able to say afterwards what the port
  kept, dropped and fixed.
- An existing understanding doc must be extended after the YAML changed.

Not for: writing a new role from scratch (no YAML to understand), or
editing telepatch itself. A primitive gap the doc finds is handed to the
telepatch repo as a request, with the shape from the doc; this skill does
not build it.

## Where the artifacts live

All state is per playbook repo, never in this skill:

```
conversion/
  SOURCE                   repo, commit and sha256 of each mirrored original
  ansible/<original path>  byte copies of the originals, read-only
  <playbook>.md            one understanding doc per playbook
```

- One doc per playbook. A role gets its own doc only when it does not fit
  in the playbook's doc (several playbooks apply it, or it is large enough
  that the playbook doc would bury the playbook). Say in the playbook doc
  which role docs exist.
- Cite originals as `file:line` relative to `conversion/ansible/`.
- A role leaves `conversion/` when its telepatch version passes
  equivalence. Its understanding then moves into code comments next to
  what implements it (one comment per kept fact, naming the reason, not
  the YAML line), and the doc is deleted with the mirrored originals that
  no other doc cites. `just check-conversion` in the playbook repo checks
  what remains.

## The levels

An understanding doc is written in ten sections, in this order, and each
is reviewed before the next is relied on:

| # | Heading | Says |
|---|---|---|
| 1 | `## Purpose` | What the playbook does for whom; who else applies its roles |
| 2 | `## Outcomes` | The end state, as names the oracle can check, per distro family |
| 3 | `## Task intent` | One row per task. The tool fills Task, Method and Signals (`tp classify --intent-skeleton`); you fill Intent, Shape, Adequate and Primitive from the YAML |
| 4 | `## Current method` | Generated by `tp classify`, pasted unedited; its patterns compared with your Shape column |
| 5 | `## Preserved knowledge` | What the YAML knows that a port must keep |
| 6 | `## Is the method current` | Dated research on whether the way it is done still holds |
| 7 | `## Recommendation and decision` | Options, primitive gaps, position, stakes, and one `Decision:` line |
| 8 | `## Port notes` | Filled during phase 2 |
| 9 | `## Review` | Dated passes and what each changed |
| 10 | `## skill-feedback` | Trial docs only |

Outcomes is built from the YAML by hand, which forces concrete names.
Task intent starts from the tool's skeleton so the inventory and the
citations are right, and the four judgment columns are yours: the
skeleton's Signals are observations ("downloads from the network and
executes it", "reports changed every run"), never a shape or a verdict.
Current method, the tool's pattern reading, is pasted afterwards and
compared with your Shape column; where they differ, yours wins and the
difference is skill feedback. `tp check-understanding <doc> --playbook
<mirrored playbook>` checks the mechanical rules, including that every
judgment cell is filled and the row count matches the YAML; the rest is
the reviewer's.

`tool` is this skill's proposal for the installer shape. The design note
(`docs/execution-model.html`) lists `repo`, `svc`, `user`, `fetch`,
`git` and `secret` as designed primitives and does not yet name `tool`;
until it does, the shape in `references/UNDERSTAND.md` level 3 is the
definition.

## Rules for the doc

- **Dated sources.** Every source is one line of the form
  `Source: <url> (accessed YYYY-MM-DD)`. A line with a URL and no date
  fails the check; a URL inside backticks (a command, a table cell) is
  not a source and is exempt. The check is per line, so keep a code
  span that holds a URL on one line. Prefer vendor documentation and issue threads; mark a
  blog or forum as secondary in the text; record a failed fetch instead of
  retrying the same URL. A claim that rests on one secondary source needs
  a second primary page, or is stated as unconfirmed under an Open line.
- **No hedge words.** The check fails on probably, might, maybe, perhaps,
  seems, likely, could be, possibly, arguably, somewhat, presumably,
  apparently. Doubt is a fact about the work, so say what is known and
  write the rest as an `Open:` line phrased as a command whose output
  settles it ("`docker run ... ls /usr/lib*/python3*/EXTERNALLY-MANAGED`
  on each image"). Generated text and fenced code are not checked.
- **One row per task in Task intent.** Includes and loops are rows too.
  A role another doc already covers is left out with `--skip-role` on
  both the skeleton and the check, and the doc says which doc covers it.
  A row's "adequate" column is yes or no with the reason; "no" rows are
  what phase 2 fixes or flags, so an empty column is a missed task, not a
  clean one. A defect in a built primitive that the role trips over (a
  group name `pkg` renders wrongly on one distro) is not a gap; it goes
  under "Built-primitive defects" with the probe that shows it.
- **Questions carry their context.** A question to the reviewer states the
  situation, the options with what speaks for and against each, the
  agent's position, and the stakes (what changes in the port or in the
  fleet if the answer goes the other way). A reviewer who did not watch the
  work must be able to answer it from the question alone.
- **Cite absences.** For a fact defined by absence (a distro missing from
  a flag list, no `update_cache`, no `creates`, no version probe) cite the
  line that would contain it and write "omits" or "no X".
- **Requested names, installed names.** Outcomes lists the names the YAML
  requests. The oracle compares installed names, which can differ
  (`gnupg` resolves through provides on RedHat). Say so wherever it
  applies.
- **Equivalence first, with the bugs named.** Branches no image reaches
  are dropped and recorded. Bugs the oracle cannot see are fixed in the
  port and listed. Changes that would show in the oracle's diff are
  recorded under "after equivalence". A primitive gap is none of these:
  it is its own list, with a recommendation to build first or port with a
  marked hack.

## Modes

- **Interactive.** The agent writes a level, stops, and the person reviews
  it; comments are folded in and the pass is dated in `## Review`. The
  `Decision:` line records who decided and when.
- **Agent-alone.** The agent writes all levels in one run, takes its own
  recommendation, and the `Decision:` line says
  `Decided by agent (agent-alone); not yet reviewed by the user.` Nothing
  else changes: the same checks apply and the person can review later. The
  port (phase 2) may proceed, and every port decision that was the agent's
  is listed in `## Port notes`.

## Procedure

1. Mirror the originals under `conversion/ansible/` and record them in
   `conversion/SOURCE` (repo, commit, sha256 per file). Hook-safe form:
   `git -C <ansible> archive <commit> <paths...> | tar -x -C
   conversion/ansible` (no shell redirects to computed paths).
2. Copy `templates/understanding.md` to `conversion/<playbook>.md`.
3. Follow `references/UNDERSTAND.md` level by level. Run `tp classify`
   on the mirrored copy (`conversion/ansible/<playbook>` with
   `--roles-path conversion/ansible/roles`) so its citations share the
   doc's root.
4. `tp check-understanding conversion/<playbook>.md --playbook
   conversion/ansible/<playbook> --roles-path conversion/ansible/roles`
   until it prints `ok`.
5. Review (interactive) or record the decision (agent-alone).
6. If the decision is "build a primitive first", stop here and hand the
   shape to the telepatch repo. Otherwise follow `references/PORT.md`:
   `tp import`, resolve the TODOs, rewrite, `cargo build`, `tp-basic
   plan`, plan snapshots on all 13 distros, `ANSIBLE_SHA=<SOURCE commit>
   just test-container`, present the diff.
7. When equivalence passes, move the understanding into code comments and
   remove the role from `conversion/`.

## Pitfalls

- Porting from the classifier output alone. It says what the YAML does,
  not what it is for or what ends up installed; Outcomes and Task intent
  are still hand-built.
- Reading `shell:` as "a shell line". It is whatever shape the command
  stands in for. The question is never "how do I run this", it is "what
  state is this trying to reach, and what would check it".
- Letting `creates`/`unless` pass for a primitive. They make a hack
  idempotent; they do not give the engine the state. Mark the hack.
- Citing only the playbook. Flags and variables live in files it never
  names (`inventories/group_vars/all`); open that first.
- Fixing in the same step as porting. A fix that shows in the oracle diff
  cannot be told from a port bug.
- Filling a section to fill it. If there are no bugs the oracle cannot
  see, write "none found". Invented findings cost the reviewer more than
  an empty section.
- Trusting the emitter. It never guesses, but it copies: dead branches,
  duplicate package names, and a `changed_if` text the YAML never
  matched all come through. `if false /* TODO */` is inert code, not a
  port. A role with mostly TODOs (`rust_build`) is a redesign.
- Running the equivalence test against ansible `master` for a port of
  an older commit. Set `ANSIBLE_SHA` to the `conversion/SOURCE` commit.
- Accepting a plan snapshot to make a test pass. Read it against
  Outcomes first; on a re-port, a changed snapshot is a failure.
- Treating this skill as finished. v3 added Task intent and the primitive
  gap rule after a port reproduced an unguarded, unpinned installer as a
  `sh!` because nobody had asked what the task was; a blind trial on
  `dev_base.yml` then caught it and nine skill-feedback items shaped the
  skeleton flow. Record what got in the way in a `## skill-feedback`
  section of the trial doc.

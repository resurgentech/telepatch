# Phase 1: understand

Fill `conversion/<playbook>.md` from `templates/understanding.md`, one
level at a time, in the order below. In interactive mode stop after each
level for review; in agent-alone mode go through all of them. After any
edit run `tp check-understanding conversion/<playbook>.md`.

Run from the telepatch checkout (`tp` is `cargo run -q -p tp --`).

The order matters. Levels 1 to 3 are written from the YAML with nothing
but your own reading. Level 4 is the tool's reading, pasted in afterwards
so the two can be compared. Writing level 3 after level 4 produces a doc
that restates the classifier, which is the failure this skill exists to
prevent.

## 1. Purpose

- Read the playbook, then the flag and variable files it never names
  (`inventories/group_vars/all`, role `defaults/`). Cite the flag lines.
- Who runs it: image definitions, other playbooks, inventories.
- Shared roles: `grep -rn "<role>" playbooks/`. Name every playbook that
  applies the role; a later playbook that re-applies it matters to the
  port (the role runs again on every image above it).

## 2. Outcomes

Written from the YAML, before the classifier output exists.

- One subsection per branch (distro family, flag). Under each: packages
  by manager with their requested names, repos added or not, tools
  installed or absent, files and users touched. Cite each name.
- An "every host" subsection for what all branches share.
- A table mapping each item to the oracle's checklist (package names,
  pip names, tool state, source lists, `/usr/local/bin` names).
- The list is requested names. The oracle compares installed names, which
  can differ when the manager resolves provides. Say so where it applies.
- Facts defined by absence are cited at the line that would hold them:
  "omits Amazon", "no `update_cache`", "no `creates`".

## 3. Task intent

One table row per task, in YAML order, includes and loops included. This
is the level where you understand the role. Write it from the YAML, in
your own words, before running the classifier.

| Column | What goes in it |
|---|---|
| Task | `file:line` and the YAML name |
| Intent | One sentence: the end state this task exists to reach. Not what it runs. "uv present in `/usr/local/bin`" not "runs the uv installer" |
| Shape | One of: `package`, `repo`, `file`, `service`, `user`, `installer`, `fetch`, `git`, `check`, `derived fact`, `dispatch` (an include or `when` that picks a branch), `glue` (a command with no state worth modelling). Name the shape the intent has, not the module the YAML used |
| Method | How the YAML reaches it, in a phrase: the module and the arguments that matter (`shell: curl \| sh, unpinned, no creates`) |
| Adequate | `yes`, or `no:` and the reason. The test is whether the method can tell when the end state is already reached, and whether it reaches the *intended* state rather than whatever the source serves today |
| Primitive | The telepatch primitive that fits the shape: `pkg`, `file`, `sh`, or one of the designed-not-built ones (`repo`, `svc`, `user`, `fetch`, `git`, `secret`, `tool`), or `none` with a one-line sketch of what it would need |

Shapes that are easy to misread:

- **installer** (`curl | sh`, `get_url` then run, `rustup`, `cargo
  install`, `pip install` outside the package manager, any task with
  `creates:` pointing at a binary). Its state has five parts: the binary
  path, the version installed (and the command that prints it), the
  version wanted (a pin, or "latest" from a named source), the source of
  truth for "latest" (a releases page, an API, the installer script's own
  version), and the install-versus-update commands. A method that has
  none of the five is `no: reinstalls every run, cannot detect drift,
  version is whatever the source serves`. The primitive is `tool`.
- **check** (`register` a command, `changed_when: false`, later `when:
  rc != 0`). The intent belongs to the task it guards; the check row's
  Primitive column says which task absorbs it (`unless` on that op, or a
  fact read when the branch picks between different ops).
- **derived fact** (`set_fact` from a command's output). A runtime
  variable; the row names who reads it and what fact replaces it.
- **glue** is rare. A build command (`make`, `cargo build`) whose
  output is not part of the host's configured state qualifies. A task
  that installs, configures, enables or fetches something does not,
  whatever module it uses.

After the table, a short list: **Primitive gaps**, one line per shape
with no built primitive that the role needs, naming the rows. This list
feeds level 7. If every row has a built primitive, write "none".

## 4. Current method

Run `tp classify <playbook>` and paste the output unedited over the
section, from `## Current method` through its last subsection. Add the
regeneration command at the end. The classifier says what the YAML does
(patterns, modules), not what it is for or what ends up installed, so
compare it against levels 2 and 3: every task it lists has a row in Task
intent, and the reverse; every pattern it names agrees with the row's
Shape, and where it does not, the row wins and the disagreement goes in
`## skill-feedback`. Read each dispatch task's `when` in the YAML and say
in Preserved knowledge what the condition encodes; the table does not
show it.

## 5. Preserved knowledge

Numbered items. Each is something the YAML knows that a port must keep:
a branch that exists for a reason, a name that is not what it looks like,
a missing guard, a duplicate, a dependency on the base image. State the
fact, then why it matters to the port, with the citation. Include what
the Dockerfile or the caller does that the playbook relies on.

## 6. Is the method current

Dated research, one subsection per method the YAML uses that could have
moved on (an installer one-liner, a package group name, a module's
requirements). For every `installer` row in Task intent this level also
answers: what version does the source serve today, how is a version
pinned with this installer, and what does the tool print for its own
version. Those three facts are what the `tool` primitive, or the hack
standing in for it, needs.

- Source line form: `Source: <url> (accessed YYYY-MM-DD)`, always on one
  line with the date.
- Prefer vendor docs and issue threads. Mark a blog or a forum as
  secondary in the sentence. A claim resting on one secondary page needs
  a primary page too.
- Search tools return summaries and fetch tools may return summaries of
  pages. Treat both as leads; confirm a quoted claim from the page, and
  record a failed fetch (404, 429) rather than retrying the same URL.
- Each subsection ends with a Verdict line.
- Unsettled points are `Open:` lines phrased as a command whose output
  settles it, preferably something the equivalence oracle captures anyway.
- Local probes (read-only commands run against the real images) go in a
  table with the date, the command and the result per image.

## 7. Recommendation and decision

Under `### Options`, two to three options with what speaks for and against
each. When Task intent listed primitive gaps, one option is always "build
the primitive first, then port", with its shape sketched: the builder
calls, the keys it writes, the postcondition, the probe. Another is "port
now with the hack marked". Say which the agent recommends and why; the
answer differs by role (a gap on the role's one real task argues for
building first; a gap on a side task argues for the marked hack).

Under `### Position`, the recommendation and its stakes: what changes in
the port and in what the oracle sees. Then the four lists:

- Dropped branches (no image reaches them).
- Bugs the oracle cannot see, fixed in the port. If there are none, write
  "none found". An unguarded installer, a check that never matched, a
  user looped twice: these are bugs, not style.
- After equivalence (change what the oracle sees; not made yet).
- Primitive gaps, carried from level 3, each with "build first" or
  "marked hack" and the reason.

End with one `Decision:` line. Interactive: who decided and when.
Agent-alone: take the recommendation and write
`Decided by agent (agent-alone); not yet reviewed by the user.`

Questions to the reviewer sit in this section, each with its situation,
options (for and against), the agent's position and the stakes.

## 8 to 10

`## Port notes` stays empty until phase 2. `## Review` gets a dated entry
per pass saying what was re-read and what changed. `## skill-feedback` is
for trial docs: the first docs written with a new version of this skill
carry it, with dated, numbered items about what the skill got in the way
of, and every place the classifier and Task intent disagreed. `tp
check-understanding` accepts it and requires it only with
`--require-feedback`; when present it comes last.

## Before review

- `tp check-understanding conversion/<playbook>.md` prints `ok`.
- Every citation was spot-checked against the file.
- Counts in Outcomes were rechecked by command, not by eye.
- Task intent has as many rows as the YAML has tasks (count them with
  `grep -c '^- name:'` across the role's task files, plus the playbook's
  own tasks), and no row has an empty Adequate or Primitive column.

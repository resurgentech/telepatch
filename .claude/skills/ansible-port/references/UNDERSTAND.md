# Phase 1: understand

Fill `conversion/<playbook>.md` from `templates/understanding.md`, one
level at a time, in the order below. In interactive mode stop after each
level for review; in agent-alone mode go through all of them. After any
edit run `tp check-understanding conversion/<playbook>.md`.

Run from the telepatch checkout (`tp` is `cargo run -q -p tp --`).

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

## 3. Current method

Run `tp classify <playbook>` and paste the output unedited over the
section, from `## Current method` through its last subsection. Add the
regeneration command at the end. The classifier says what the YAML does
(patterns, modules), not what ends up installed, so compare it against
Outcomes: every task it lists should appear there, and the reverse. Read
each dispatch task's `when` in the YAML and say in Preserved knowledge
what the condition encodes; the table does not show it.

## 4. Preserved knowledge

Numbered items. Each is something the YAML knows that a port must keep:
a branch that exists for a reason, a name that is not what it looks like,
a missing guard, a duplicate, a dependency on the base image. State the
fact, then why it matters to the port, with the citation. Include what
the Dockerfile or the caller does that the playbook relies on.

## 5. Is the method current

Dated research, one subsection per method the YAML uses that could have
moved on (an installer one-liner, a package group name, a module's
requirements).

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

## 6. Recommendation and decision

Under `### Options`, two to three options with what speaks for and against
each. Under `### Position`, the recommendation and its stakes: what
changes in the port and in what the oracle sees. Apply the Open question
3 default:

- Equivalence on the images comes first.
- Branches no image reaches are dropped and listed under "Dropped
  branches".
- Bugs the oracle cannot see are fixed and listed. If there are none,
  write "none found".
- Changes that would show in the oracle's diff go under "After
  equivalence" and are not made yet.

End with one `Decision:` line. Interactive: who decided and when.
Agent-alone: take the recommendation and write
`Decided by agent (agent-alone); not yet reviewed by the user.`

Questions to the reviewer sit in this section, each with its situation,
options (for and against), the agent's position and the stakes.

## 7 to 9

`## Port notes` stays empty until phase 2. `## Review` gets a dated entry
per pass saying what was re-read and what changed. `## skill-feedback` is
for trial docs: the first docs written with a new version of this skill
carry it, with dated, numbered items about what the skill got in the way
of. `tp check-understanding` accepts it and requires it only with
`--require-feedback`; when present it comes last.

## Before review

- `tp check-understanding conversion/<playbook>.md` prints `ok`.
- Every citation was spot-checked against the file.
- Counts in Outcomes were rechecked by command, not by eye.

# Phase 2: port

Start only when the doc's `Decision:` line exists. Port what it decided;
record deviations in `## Port notes`.

Built today: the `#[role]` macro, `Host` and its facts, `h.pkg` and
friends, `h.file`, `sh!`, `as_user`, `barrier`, and the planner types.
Not built yet, and written as a note in the role instead of faked:

- Guards and `notify` (handlers become a guard on the restart op).
- `repo`, `svc`, `user`, `fetch`, `tpl!`, `git`.
- The `plan` and `run` commands (T12). Until then a role's trace is
  inspected through the library.
- The equivalence harness (T18, in `tp_basic`).
- `tp import`, which will generate a first-draft role from the YAML
  (T22). Today the mapping below is applied by hand.

## Mapping

| Ansible | telepatch |
|---|---|
| `apt`/`dnf`/`pacman` with `name:` list | `h.pkg(["a", "b"])`: manager from `h.os.family` (Debian apt, RedHat dnf, Arch pacman) |
| `yum` | `h.yum([..])` |
| `apt` with `state: build-dep` | `h.pkg([..]).build_dep()` |
| `state: latest` | `h.pkg([..]).latest()` |
| `update_cache: yes/no` | `.update_cache(true)`; the engine already refreshes the apt cache once before the first install, so drop per-task flags unless the YAML relies on `no` |
| `pip` | `h.pip([..])`, `.latest()` for `state: latest` |
| `lineinfile` | `h.file(p).line("..")`; with `regexp:` `h.file(p).line_re("^re", "..")` |
| `copy` with `content:` | `h.file(p).content("..")`, then `.mode(0o644)` and `.owner("u")` if the YAML sets them |
| `file` `state: directory` / `absent` | `h.file(p).dir()` / `.absent()` |
| `shell`/`command` | `sh!(h, "cmd {var}")`; `{var}` is quoted by the macro, `{{` and `}}` are literal braces |
| `args: creates:` | `sh!(h, "..", creates = "path")` |
| `register` + `changed_when: false` + `when: rc != 0` (check, then act) | one `sh!(h, "act", unless = "check")` |
| `changed_when: "<regex>" in stdout` | `sh!(h, "..", changed_if = "regex")` |
| `ignore_errors: yes`, `failed_when: false` | `.allow_fail()` |
| task knows what it touches (a shell line that installs a package) | `.writes([Key::pkg("x")])`, `.reads([..])` |
| `loop: "{{ admin_users }}"` with `become_user: "{{ item }}"` | `for u in &h.vars.admin_users { let s = h.as_user(u); sh!(s, "..") }`; ops on `s` run in a fresh login environment of `u` |
| `become: yes` (root, no user) | the host handle itself (the run is root) |
| `include_tasks`/`when:` on distro flags (`debian_based`, `redhat_based`, ...) | `match h.os.family { Family::Debian => .., Family::RedHat => .., Family::Arch => .., Family::Other(_) => .. }` in the role; use `h.os.distro` for a single distro (Amazon is its own distro, outside RedHat only if the YAML keeps it out) |
| `when: ansible_distribution_major_version == ..` | a `h.os` version test in the `match` arm |
| `when:` on a command's output | `h.bin_version("cargo").await`, `h.exists(p).await`, `h.bin_exists(c).await`: facts are awaited, ops are not |
| `roles:` / `import_role` | one `#[role] async fn name<V>(h: &Host<V>)` per role, called in order |
| ordering that must hold across roles | `h.barrier()` |
| `vars`/`defaults` | fields of the vars struct behind `h.vars` |
| `notify` / `handlers` | not built: write a `// TODO(guard)` where the restart goes and keep the order |
| `git`, `service`, `user`, `get_url`/`uri`, `template` | not built: use `sh!` with `unless`/`creates` and say in Port notes which primitive replaces it later |

### Sketch

```rust
#[role]
async fn dev_common(h: &Host<Vars>) -> Result<(), TraceError> {
    match h.os.family {
        Family::Debian => { h.pkg(["build-essential", "cmake"]); }
        Family::RedHat => { h.pkg(["cmake", "gcc"]); }
        Family::Arch => { h.pkg(["base-devel", "cmake"]); }
        Family::Other(_) => {}
    }
    sh!(
        h,
        "curl -LsSf https://astral.sh/uv/install.sh | UV_INSTALL_DIR=/usr/local/bin sh",
        creates = "/usr/local/bin/uv"
    );
    Ok(())
}
```

(The `creates` guard is an "after equivalence" change when the YAML ran
the installer on every play; port the YAML as it is first.)

## Steps

1. For each task file, write the role function with the mapping above.
   Keep the YAML's order. Keep names the YAML requests; fix nothing that
   shows in the oracle diff.
2. Apply the doc: dropped branches stay out, bugs the oracle cannot see
   are fixed, "after equivalence" items are left as comments naming the
   item.
3. Dry-run with `plan` once T12 lands: read the plan for each of the
   images, compare its package sets and shell lines with Outcomes. Until
   then inspect `h.snapshot()` in a test.
4. Equivalence test (T18, tp_basic): run the ported role and the Ansible
   original against the same image and compare what the oracle captures.
   Record the result in `## Port notes`.
5. On pass, move each kept fact from the doc into a comment beside the
   code that implements it, and take the role out of `conversion/`.
6. Then make the "after equivalence" changes, each in its own change with
   its own oracle diff.

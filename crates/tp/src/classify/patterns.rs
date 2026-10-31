//! Pattern detection (execution-model.html "What the Ansible repo does").
//!
//! A task may match several patterns; `detect` returns every one it matches.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::modules::short;
use crate::ansible::Task;

pub struct Pattern {
    pub id: &'static str,
    pub title: &'static str,
    /// What the design says the pattern becomes.
    pub primitive: &'static str,
}

pub const PATTERNS: &[Pattern] = &[
    Pattern {
        id: "distro_dispatch",
        title: "Distro dispatch",
        primitive: "typed `os` fact and a `match`, or a per-family table on `pkg`",
    },
    Pattern {
        id: "distro_guard",
        title: "Guarded by a distro flag",
        primitive: "the task's own primitive inside the `match` arm; the flag is not a dispatch point",
    },
    Pattern {
        id: "installer",
        title: "Installer outside the package manager",
        primitive: "`tool` (binary, installed version, wanted version, source, install/update); not built",
    },
    Pattern {
        id: "per_user_loop",
        title: "Per-user loop",
        primitive: "`for u in users { let s = h.as_user(u); ... }`; the login shell is a fact of the user",
    },
    Pattern {
        id: "hand_rolled_ensure",
        title: "Hand-rolled ensure",
        primitive: "`sh!(..).unless(check)` or `.creates(path)`",
    },
    Pattern {
        id: "version_pinning",
        title: "Version pinning",
        primitive: "`bin_version` fact plus the ensure combinator",
    },
    Pattern {
        id: "repo_setup",
        title: "Repo setup",
        primitive: "`repo` primitive; one cache update per window",
    },
    Pattern {
        id: "shell_for_primitive",
        title: "Shell for a primitive",
        primitive: "the matching primitive (`svc`, `file`, `pkg`, `user`), not `sh`",
    },
    Pattern {
        id: "controller_side",
        title: "Controller-side gather",
        primitive: "`secret`, `fetch`, `local` (prepare phase)",
    },
    Pattern {
        id: "handler",
        title: "Handler",
        primitive: "a guard on the restart op; identical guarded ops merge",
    },
];

static DISTRO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(\w+_based|ansible_(distribution\w*|os_family|pkg_mgr|facts)|ansible_facts\b)")
        .unwrap()
});

static SHELL_PRIMITIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?m)(?:^|[;&|(]|&&|\|\|)\s*(?:sudo\s+)?(?P<cmd>systemctl|service\s+\S+\s+(?:start|stop|restart)|mkdir|ln\s+-\w*s|chmod|chown|touch|rm\s|cp\s|mv\s|tee\s|sed\s+-\w*i|useradd|usermod|adduser|groupadd|apt(?:-get)?\s+install|dnf\s+install|yum\s+install|pacman\s+-S|update-grub|echo\s[^\n]*>)",
    )
    .unwrap()
});

static REPO_SHELL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"gpg\s+--dearmor|apt-key|add-apt-repository|sources\.list|yum-config-manager|dnf\s+config-manager|yum\.repos\.d|/etc/apt/keyrings|/usr/share/keyrings",
    )
    .unwrap()
});

static VERSION_CMD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"--version|\s-V\b|\bversion\b").unwrap());

static ANSIBLE_FACT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"ansible_\w+").unwrap());

static CMP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!=|==|version_compare|is version").unwrap());

/// Text of the command for `shell`/`command`/`raw`/`script`.
pub fn command_text(t: &Task) -> Option<String> {
    if !matches!(short(&t.module), "shell" | "command" | "raw" | "script") {
        return None;
    }
    match &t.args {
        Value::String(s) => Some(s.clone()),
        Value::Object(m) => m
            .get("cmd")
            .or_else(|| m.get("_raw_params"))
            .and_then(Value::as_str)
            .map(String::from),
        _ => None,
    }
}

fn all_whens(t: &Task) -> impl Iterator<Item = &String> {
    t.when.iter().chain(&t.inherited_when)
}

/// True if `cond` reads `reg` in a way that tests for failure.
fn tests_failure(cond: &str, reg: &str) -> bool {
    let c: String = cond.chars().filter(|c| !c.is_whitespace()).collect();
    [".rc!=0", ".rc>0", ".failed", "isfailed", "isnotsuccess"]
        .iter()
        .any(|form| has_word(&c, &format!("{reg}{form}")))
}

/// `needle` occurs in `hay` not preceded by an identifier character.
fn has_word(hay: &str, needle: &str) -> bool {
    hay.match_indices(needle).any(|(i, _)| {
        !hay[..i]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

fn is_check(t: &Task) -> Option<&str> {
    let reg = t.register.as_deref()?;
    let quiet = t.changed_when.as_deref() == Some("false")
        || matches!(
            t.ignore_errors
                .as_deref()
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("yes" | "true")
        );
    quiet.then_some(reg)
}

fn args_text(t: &Task) -> String {
    t.args.to_string()
}

fn looped_users(expr: &str) -> bool {
    let e = expr.to_ascii_lowercase();
    e.contains("user") || e.contains("root")
}

pub fn shell_primitive(t: &Task) -> Option<&'static str> {
    let cmd = command_text(t)?;
    let stripped: String = cmd
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let m = SHELL_PRIMITIVE.captures(&stripped)?;
    let c = m.name("cmd")?.as_str();
    Some(if c.starts_with("systemctl") || c.starts_with("service") {
        "svc"
    } else if c.starts_with("useradd")
        || c.starts_with("usermod")
        || c.starts_with("adduser")
        || c.starts_with("groupadd")
    {
        "user"
    } else if c.contains("install") || c.starts_with("pacman") {
        "pkg"
    } else if c.starts_with("update-grub") {
        "sh (declared effects)"
    } else {
        "file"
    })
}

fn is_repo_setup(t: &Task) -> bool {
    let m = short(&t.module);
    match m {
        "apt_key" | "apt_repository" | "yum_repository" | "rpm_key" | "deb822_repository"
        | "dnf_repository" => true,
        "apt" | "dnf" | "yum" => {
            let a = t.args.as_object();
            let has = |k: &str| a.is_some_and(|a| a.contains_key(k));
            let update = a.and_then(|a| a.get("update_cache")).is_some_and(|v| {
                v.as_bool() == Some(true) || matches!(v.as_str(), Some("yes" | "true"))
            });
            update && !has("name") && !has("pkg") && !has("upgrade")
        }
        "copy" | "template" | "file" | "lineinfile" | "get_url" => {
            let text = args_text(t);
            text.contains("sources.list")
                || text.contains("yum.repos.d")
                || text.contains("/etc/apt/keyrings")
        }
        _ => command_text(t).is_some_and(|c| REPO_SHELL.is_match(&c)),
    }
}

fn is_local(target: &str) -> bool {
    matches!(target, "localhost" | "127.0.0.1" | "::1")
}

/// A delegate target that is not the controller: out of scope for now.
pub fn delegates_elsewhere(t: &Task) -> Option<&str> {
    let d = t.delegate_to.as_deref()?;
    (!is_local(d) && d != "{{ inventory_hostname }}").then_some(d)
}

/// Indices into [`PATTERNS`] matched by each task, in input order.
pub fn detect(tasks: &[&Task]) -> Vec<Vec<usize>> {
    let mut hits: Vec<Vec<usize>> = vec![Vec::new(); tasks.len()];
    let id = |name: &str| PATTERNS.iter().position(|p| p.id == name).unwrap();
    let (dd, dg, ins, pu, he, vp, rs, sp, cs, hd) = (
        id("distro_dispatch"),
        id("distro_guard"),
        id("installer"),
        id("per_user_loop"),
        id("hand_rolled_ensure"),
        id("version_pinning"),
        id("repo_setup"),
        id("shell_for_primitive"),
        id("controller_side"),
        id("handler"),
    );

    for (i, t) in tasks.iter().enumerate() {
        // Distro dispatch: an include or import whose own `when` tests a
        // distro flag or fact. The same condition on any other task is a
        // guard on that task, not a dispatch point.
        let is_include =
            short(&t.module).contains("include") || short(&t.module).contains("import");
        if t.when.iter().any(|w| DISTRO.is_match(w)) {
            hits[i].push(if is_include { dd } else { dg });
        }
        // Installer: a remote script piped to a shell, a tool installer
        // outside the package manager, or `creates:` pointing at a binary.
        let cmd = command_text(t).unwrap_or_default();
        let creates_bin = t
            .kw_args
            .get("creates")
            .and_then(Value::as_str)
            .is_some_and(|c| c.contains("/bin/"));
        if !super::signals::is_probe(t)
            && (super::signals::NET_TO_SH.is_match(&cmd)
                || super::signals::TOOL_INSTALLERS.is_match(&cmd)
                || creates_bin)
        {
            hits[i].push(ins);
        }
        // Per-user loop: a looped include over users, anything under one, or
        // a task that becomes a templated user.
        let own_loop = short(&t.module).contains("include")
            && t.loop_expr.as_deref().is_some_and(looped_users);
        let under = t
            .include_chain
            .iter()
            .any(|f| f.loop_expr.as_deref().is_some_and(looped_users));
        let templated_user = t.become_user.as_deref().is_some_and(|u| u.contains("{{"));
        if own_loop || under || templated_user {
            hits[i].push(pu);
        }
        // Version pinning.
        let ver_cmd = command_text(t).is_some_and(|c| VERSION_CMD.is_match(&c));
        // `ansible_distribution_version` etc. are distro facts, not pins.
        let ver_cmp = |s: &str| {
            let s = ANSIBLE_FACT.replace_all(s, "");
            s.contains("version") && CMP.is_match(&s)
        };
        let ver_fact = short(&t.module) == "set_fact" && ver_cmp(&args_text(t));
        let ver_when = all_whens(t).any(|w| ver_cmp(w));
        if ver_cmd || ver_fact || ver_when {
            hits[i].push(vp);
        }
        if is_repo_setup(t) {
            hits[i].push(rs);
        }
        if shell_primitive(t).is_some() {
            hits[i].push(sp);
        }
        let lookup = args_text(t).contains("lookup(");
        if t.delegate_to.as_deref().is_some_and(is_local) || lookup {
            hits[i].push(cs);
        }
        if t.is_handler || !t.notify.is_empty() {
            hits[i].push(hd);
        }
    }

    // Hand-rolled ensure: a quiet registered check paired with a later task
    // in the same file whose `when` tests that register for failure.
    for (i, t) in tasks.iter().enumerate() {
        let Some(reg) = is_check(t) else { continue };
        let acts: Vec<usize> = (i + 1..tasks.len())
            .filter(|&j| {
                tasks[j].file == t.file && all_whens(tasks[j]).any(|w| tests_failure(w, reg))
            })
            .collect();
        if !acts.is_empty() {
            for k in std::iter::once(i).chain(acts) {
                if !hits[k].contains(&he) {
                    hits[k].push(he);
                }
            }
        }
    }
    for h in &mut hits {
        h.sort_unstable();
    }
    hits
}

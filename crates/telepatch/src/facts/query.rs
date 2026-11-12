//! Fact questions as data: what is asked, which effect key it reads, the
//! shell probe the real executor runs, and how a postcondition answers it.

use std::fmt;

use crate::facts::os::{Family, Os};
use crate::key::{Check, Key, Stat, UserInfo, Value};
use crate::trace::RunAs;

/// A question about a host that the playbook awaits.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Fact {
    /// `exists(path)` -> `Value::Exists(bool)`.
    Exists(String),
    /// `bin_exists(cmd)` -> `Value::Exists(bool)`.
    BinExists(String),
    /// `bin_version(cmd)` -> `Value::Text(first line)` or `Value::Absent`.
    BinVersion(String),
    /// `user_info(name)` -> `Value::User`.
    UserInfo(String),
    /// `stat(path)` -> `Value::Stat`.
    Stat(String),
    /// `pkg_installed(name)` -> `Value::Pkg`.
    PkgInstalled(String),
    /// `check(cmd)` -> `Value::Check`. The command is a shell snippet.
    Check(String),
}

impl Fact {
    pub fn exists(path: impl Into<String>) -> Fact {
        Fact::Exists(path.into())
    }
    pub fn bin_exists(cmd: impl Into<String>) -> Fact {
        Fact::BinExists(cmd.into())
    }
    pub fn bin_version(cmd: impl Into<String>) -> Fact {
        Fact::BinVersion(cmd.into())
    }
    pub fn user_info(name: impl Into<String>) -> Fact {
        Fact::UserInfo(name.into())
    }
    pub fn stat(path: impl Into<String>) -> Fact {
        Fact::Stat(path.into())
    }
    pub fn pkg_installed(name: impl Into<String>) -> Fact {
        Fact::PkgInstalled(name.into())
    }
    pub fn check(cmd: impl Into<String>) -> Fact {
        Fact::Check(cmd.into())
    }

    /// The effect key this fact reads. Writes that overlap it invalidate a
    /// confirmed answer and can be the writer a postcondition answers from.
    /// `exists(p)` reads `file:p`; `bin_*` read `bin:cmd`, which `*`, any
    /// `pkg:` and any `file:` key overlap (see [`Key::overlaps`]).
    /// `check` reads `*`: a general command can observe anything already
    /// traced, so pending mutations run before it.
    pub fn effect_key(&self) -> Key {
        match self {
            Fact::Exists(p) | Fact::Stat(p) => Key::file(p),
            Fact::BinExists(c) | Fact::BinVersion(c) => Key::bin(c),
            Fact::UserInfo(n) => Key::user(n),
            Fact::PkgInstalled(n) => Key::pkg(n),
            Fact::Check(_) => Key::Any,
        }
    }

    /// Shell script the executor runs (in the fact's RunAs shell) to answer
    /// it. Output is fed to [`Fact::parse`]. Package facts need the host
    /// family; [`Fact::probe_for`] supplies it. [`Fact::probe`] is the same
    /// script with no family, which tries apt, then rpm, then pacman.
    pub fn probe(&self) -> String {
        self.script(None)
    }

    /// [`Fact::probe`] for `os`. `pkg_installed` asks that family's manager.
    pub fn probe_for(&self, os: &Os) -> String {
        self.script(Some(os))
    }

    fn script(&self, os: Option<&Os>) -> String {
        match self {
            Fact::Exists(p) => {
                format!("if test -e {}; then echo yes; else echo no; fi", quote(p))
            }
            Fact::BinExists(c) => format!(
                "if command -v {} >/dev/null 2>&1; then echo yes; else echo no; fi",
                quote(c)
            ),
            Fact::BinVersion(c) => format!(
                "if command -v {c} >/dev/null 2>&1; then echo present; {c} --version 2>&1 | head -n 1; else echo missing; fi",
                c = quote(c)
            ),
            Fact::UserInfo(n) => user_probe(n),
            Fact::Stat(p) => stat_probe(p),
            Fact::PkgInstalled(n) => pkg_probe(n, os.map(|o| &o.family)),
            Fact::Check(c) => check_probe(c),
        }
    }

    /// What a failed or skipped read answers with.
    pub fn missing(&self) -> Value {
        match self {
            Fact::BinVersion(_) => Value::Absent,
            Fact::UserInfo(_) => Value::User(UserInfo {
                home: String::new(),
                shell: String::new(),
                exists: false,
            }),
            Fact::Stat(_) => Value::Stat(Stat {
                mode: String::new(),
                owner: String::new(),
                size: 0,
                exists: false,
            }),
            Fact::PkgInstalled(_) => Value::Pkg(None),
            Fact::Check(_) => Value::Check(Check {
                rc: 1,
                stdout: String::new(),
            }),
            Fact::Exists(_) | Fact::BinExists(_) => Value::Exists(false),
        }
    }

    /// Turn the probe's stdout into the fact's value.
    pub fn parse(&self, stdout: &str) -> Value {
        if let Fact::Check(_) = self {
            return parse_check(stdout);
        }
        let mut lines = stdout.lines();
        let first = lines.next().unwrap_or("").trim();
        match self {
            Fact::Exists(_) | Fact::BinExists(_) => Value::Exists(first == "yes"),
            Fact::BinVersion(_) => {
                if first == "present" {
                    Value::Text(lines.next().unwrap_or("").trim().to_string())
                } else {
                    Value::Absent
                }
            }
            Fact::UserInfo(_) => {
                if first == "exists" {
                    Value::User(UserInfo {
                        home: lines.next().unwrap_or("").trim().to_string(),
                        shell: lines.next().unwrap_or("").trim().to_string(),
                        exists: true,
                    })
                } else {
                    self.missing()
                }
            }
            Fact::Stat(_) => {
                if first == "exists" {
                    let mode = lines.next().unwrap_or("").trim().to_string();
                    let owner = lines.next().unwrap_or("").trim().to_string();
                    let size = lines.next().unwrap_or("").trim().parse().unwrap_or(0);
                    Value::Stat(Stat {
                        mode,
                        owner,
                        size,
                        exists: true,
                    })
                } else {
                    self.missing()
                }
            }
            Fact::PkgInstalled(_) => {
                if let Some(ver) = first.strip_prefix("installed ") {
                    Value::Pkg(Some(ver.trim().to_string()))
                } else if first == "installed" {
                    Value::Pkg(Some(String::new()))
                } else {
                    Value::Pkg(None)
                }
            }
            Fact::Check(_) => unreachable!("handled above"),
        }
    }

    /// What a writer's postcondition value for this fact's effect key says
    /// about the fact, or `None` when it does not determine it.
    pub fn from_post(&self, post: &Value) -> Option<Value> {
        match (self, post) {
            (Fact::Exists(_) | Fact::BinExists(_), Value::Exists(b)) => Some(Value::Exists(*b)),
            (Fact::BinExists(_), Value::Text(_)) => Some(Value::Exists(true)),
            (Fact::BinExists(_), Value::Absent) => Some(Value::Exists(false)),
            (Fact::BinVersion(_), Value::Text(t)) => Some(Value::Text(t.clone())),
            (Fact::BinVersion(_), Value::Exists(false) | Value::Absent) => Some(Value::Absent),
            _ => None,
        }
    }
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

impl fmt::Display for Fact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fact::Exists(p) => write!(f, "exists({p})"),
            Fact::BinExists(c) => write!(f, "bin_exists({c})"),
            Fact::BinVersion(c) => write!(f, "bin_version({c})"),
            Fact::UserInfo(n) => write!(f, "user_info({n})"),
            Fact::Stat(p) => write!(f, "stat({p})"),
            Fact::PkgInstalled(n) => write!(f, "pkg_installed({n})"),
            Fact::Check(c) => write!(f, "check({c})"),
        }
    }
}

fn user_probe(name: &str) -> String {
    let q = quote(name);
    format!(
        "line=$(getent passwd {q} 2>/dev/null) || line=\n\
         if [ -n \"$line\" ]; then\n\
         echo exists\n\
         shell=${{line##*:}}\n\
         rest=${{line%:*}}\n\
         home=${{rest##*:}}\n\
         printf '%s\\n' \"$home\" \"$shell\"\n\
         else\n\
         echo missing\n\
         fi\n"
    )
}

fn stat_probe(path: &str) -> String {
    let q = quote(path);
    format!(
        "if [ -e {q} ] || [ -L {q} ]; then\n\
         echo exists\n\
         stat -c '%a' -- {q}\n\
         stat -c '%U' -- {q}\n\
         stat -c '%s' -- {q}\n\
         else\n\
         echo missing\n\
         fi\n"
    )
}

fn apt_ver(q: &str) -> String {
    format!(
        "st=$(dpkg-query -W -f='${{db:Status-Status}}' {q} 2>/dev/null) || st=\n\
         if [ \"$st\" = installed ]; then\n\
         ver=$(dpkg-query -W -f='${{Version}}' {q} 2>/dev/null) || ver=\n\
         printf 'installed %s\\n' \"$ver\"\n\
         else echo absent; fi\n"
    )
}

fn rpm_ver(q: &str) -> String {
    format!(
        "if ver=$(rpm -q --qf '%{{EVR}}' {q} 2>/dev/null); then\n\
         printf 'installed %s\\n' \"$ver\"\n\
         else echo absent; fi\n"
    )
}

fn pacman_ver(q: &str) -> String {
    format!(
        "if line=$(pacman -Q {q} 2>/dev/null); then\n\
         printf 'installed %s\\n' \"${{line#* }}\"\n\
         else echo absent; fi\n"
    )
}

/// Ask the family's manager. With no family, the first of apt, rpm and
/// pacman that reports the name installed wins.
fn pkg_probe(name: &str, family: Option<&Family>) -> String {
    let q = quote(name);
    match family {
        Some(Family::Debian) => apt_ver(&q),
        Some(Family::RedHat) => rpm_ver(&q),
        Some(Family::Arch) => pacman_ver(&q),
        Some(Family::Other(_)) | None => format!(
            "tp_pkg=$({apt})\n\
             if [ \"$tp_pkg\" = absent ]; then tp_pkg=$({rpm}); fi\n\
             if [ \"$tp_pkg\" = absent ]; then tp_pkg=$({pac}); fi\n\
             printf '%s\\n' \"$tp_pkg\"\n",
            apt = apt_ver(&q),
            rpm = rpm_ver(&q),
            pac = pacman_ver(&q),
        ),
    }
}

/// Run `cmd` in a fresh bash. Stdout is preserved, including a trailing
/// newline; stderr is discarded so it does not join the shell's merged
/// stream. The temp file is removed before the probe returns.
fn check_probe(cmd: &str) -> String {
    let c = quote(cmd);
    format!(
        "tp_f=$(mktemp 2>/dev/null) || tp_f=\n\
         if [ -z \"$tp_f\" ]; then printf '%s\\n%s\\n' 1 0; else\n\
         bash -c {c} >\"$tp_f\" 2>/dev/null\n\
         tp_rc=$?\n\
         printf '%s\\n' \"$tp_rc\"\n\
         wc -c < \"$tp_f\" | tr -d '[:space:]'\n\
         printf '\\n'\n\
         cat \"$tp_f\"\n\
         rm -f -- \"$tp_f\"\n\
         fi\n"
    )
}

fn parse_check(stdout: &str) -> Value {
    let Some((rc_line, rest)) = stdout.split_once('\n') else {
        return Value::Check(Check {
            rc: 1,
            stdout: String::new(),
        });
    };
    let Some((len_line, rest)) = rest.split_once('\n') else {
        return Value::Check(Check {
            rc: rc_line.trim().parse().unwrap_or(1),
            stdout: String::new(),
        });
    };
    let rc = rc_line.trim().parse().unwrap_or(1);
    let len: usize = len_line.trim().parse().unwrap_or(0);
    let bytes = rest.as_bytes();
    let take = len.min(bytes.len());
    Value::Check(Check {
        rc,
        stdout: String::from_utf8_lossy(&bytes[..take]).into_owned(),
    })
}

/// A fact plus the shell it is asked in. Root's `which cargo` and a login
/// environment's are different facts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FactQuery {
    pub run_as: RunAs,
    pub fact: Fact,
}

impl fmt::Display for FactQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} as {}", self.fact, self.run_as)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_probe_output() {
        assert_eq!(Fact::exists("/x").parse("yes\n"), Value::Exists(true));
        assert_eq!(Fact::bin_exists("a").parse("no\n"), Value::Exists(false));
        assert_eq!(
            Fact::bin_version("a").parse("present\ncargo 1.90.0\n"),
            Value::Text("cargo 1.90.0".into())
        );
        assert_eq!(Fact::bin_version("a").parse("missing\n"), Value::Absent);
        assert_eq!(
            Fact::user_info("u").parse("exists\n/home/u\n/bin/zsh\n"),
            Value::User(UserInfo {
                home: "/home/u".into(),
                shell: "/bin/zsh".into(),
                exists: true,
            })
        );
        assert_eq!(
            Fact::user_info("u").parse("missing\n"),
            Fact::user_info("u").missing()
        );
        assert_eq!(
            Fact::stat("/x").parse("exists\n644\nroot\n12\n"),
            Value::Stat(Stat {
                mode: "644".into(),
                owner: "root".into(),
                size: 12,
                exists: true,
            })
        );
        assert_eq!(
            Fact::pkg_installed("bash").parse("installed 1.2.3-1\n"),
            Value::Pkg(Some("1.2.3-1".into()))
        );
        assert_eq!(
            Fact::pkg_installed("bash").parse("absent\n"),
            Value::Pkg(None)
        );
        assert_eq!(
            Fact::check("x").parse("0\n2\nhi"),
            Value::Check(Check {
                rc: 0,
                stdout: "hi".into(),
            })
        );
        assert_eq!(
            Fact::check("x").parse("3\n3\nhi\n"),
            Value::Check(Check {
                rc: 3,
                stdout: "hi\n".into(),
            })
        );
    }

    #[test]
    fn probe_quotes() {
        assert!(Fact::exists("/a b'c").probe().contains("'/a b'\\''c'"));
        assert!(Fact::user_info("a b").probe().contains("'a b'"));
        let deb = Fact::pkg_installed("bash")
            .probe_for(&Os::parse("ID=ubuntu\nVERSION_ID=24.04\n").unwrap());
        assert!(deb.contains("dpkg-query"), "{deb}");
        assert!(!deb.contains("pacman"), "{deb}");
        let arch = Fact::pkg_installed("bash").probe_for(&Os::parse("ID=arch\n").unwrap());
        assert!(arch.contains("pacman -Q"), "{arch}");
        assert!(!arch.contains("dpkg-query"), "{arch}");
    }

    #[test]
    fn check_probe_keeps_stdout_and_rc() {
        let run = |cmd: &str| {
            let script = Fact::check(cmd).probe();
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(&script)
                .output()
                .expect("bash");
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            Fact::check(cmd).parse(&String::from_utf8_lossy(&out.stdout))
        };
        assert_eq!(
            run("printf 'hi\\n'"),
            Value::Check(Check {
                rc: 0,
                stdout: "hi\n".into(),
            })
        );
        assert_eq!(
            run("exit 3"),
            Value::Check(Check {
                rc: 3,
                stdout: String::new(),
            })
        );
    }
}

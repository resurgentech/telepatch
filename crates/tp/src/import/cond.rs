//! `when:` conditions: a small Jinja expression parser and translator.
//!
//! Handles distro flags (`debian_based|bool`), `not`/`and`/`or`/parens,
//! `ansible_distribution ==/!=/in`, `ansible_distribution_major_version|int
//! <op> N`, and `<register>.rc != 0` (reported separately so the emitter can
//! fold it into `unless =`). Anything else is an error naming the atom.

use super::flags;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Num(i64),
    Op(&'static str),
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Pipe,
}

#[derive(Debug, Clone)]
enum Expr {
    Or(Vec<Expr>),
    And(Vec<Expr>),
    Not(Box<Expr>),
    /// `name` with an optional filter.
    Var(String, Option<String>),
    Str(String),
    Num(i64),
    List(Vec<Expr>),
    Cmp(Box<Expr>, &'static str, Box<Expr>),
    In(Box<Expr>, Box<Expr>),
}

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
        } else if ch.is_alphabetic() || ch == '_' {
            let st = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_' || c[i] == '.') {
                i += 1;
            }
            out.push(Tok::Ident(c[st..i].iter().collect()));
        } else if ch.is_ascii_digit() {
            let st = i;
            while i < c.len() && c[i].is_ascii_digit() {
                i += 1;
            }
            let n: String = c[st..i].iter().collect();
            out.push(Tok::Num(n.parse().map_err(|_| "bad number")?));
        } else if ch == '\'' || ch == '"' {
            let st = i + 1;
            i += 1;
            while i < c.len() && c[i] != ch {
                i += 1;
            }
            if i >= c.len() {
                return Err("unterminated string".into());
            }
            out.push(Tok::Str(c[st..i].iter().collect()));
            i += 1;
        } else {
            let two: String = c[i..(i + 2).min(c.len())].iter().collect();
            let t = match two.as_str() {
                "==" => Some("=="),
                "!=" => Some("!="),
                ">=" => Some(">="),
                "<=" => Some("<="),
                _ => None,
            };
            if let Some(t) = t {
                out.push(Tok::Op(t));
                i += 2;
                continue;
            }
            out.push(match ch {
                '>' => Tok::Op(">"),
                '<' => Tok::Op("<"),
                '(' => Tok::LParen,
                ')' => Tok::RParen,
                '[' => Tok::LBracket,
                ']' => Tok::RBracket,
                ',' => Tok::Comma,
                '|' => Tok::Pipe,
                other => return Err(format!("unexpected `{other}`")),
            });
            i += 1;
        }
    }
    Ok(out)
}

struct P {
    t: Vec<Tok>,
    i: usize,
}

impl P {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn kw(&mut self, k: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Ident(s)) if s == k) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn or(&mut self) -> Result<Expr, String> {
        let mut v = vec![self.and()?];
        while self.kw("or") {
            v.push(self.and()?);
        }
        Ok(if v.len() == 1 {
            v.remove(0)
        } else {
            Expr::Or(v)
        })
    }
    fn and(&mut self) -> Result<Expr, String> {
        let mut v = vec![self.not()?];
        while self.kw("and") {
            v.push(self.not()?);
        }
        Ok(if v.len() == 1 {
            v.remove(0)
        } else {
            Expr::And(v)
        })
    }
    fn not(&mut self) -> Result<Expr, String> {
        if self.kw("not") {
            Ok(Expr::Not(Box::new(self.not()?)))
        } else {
            self.cmp()
        }
    }
    fn cmp(&mut self) -> Result<Expr, String> {
        let l = self.primary()?;
        if let Some(Tok::Op(op)) = self.peek().cloned() {
            self.i += 1;
            let r = self.primary()?;
            return Ok(Expr::Cmp(Box::new(l), op, Box::new(r)));
        }
        if self.kw("in") {
            let r = self.primary()?;
            return Ok(Expr::In(Box::new(l), Box::new(r)));
        }
        Ok(l)
    }
    fn primary(&mut self) -> Result<Expr, String> {
        match self.t.get(self.i).cloned() {
            Some(Tok::LParen) => {
                self.i += 1;
                let e = self.or()?;
                if self.peek() != Some(&Tok::RParen) {
                    return Err("missing `)`".into());
                }
                self.i += 1;
                Ok(e)
            }
            Some(Tok::LBracket) => {
                self.i += 1;
                let mut v = Vec::new();
                while self.peek() != Some(&Tok::RBracket) {
                    v.push(self.primary()?);
                    if self.peek() == Some(&Tok::Comma) {
                        self.i += 1;
                    }
                    if self.peek().is_none() {
                        return Err("missing `]`".into());
                    }
                }
                self.i += 1;
                Ok(Expr::List(v))
            }
            Some(Tok::Str(s)) => {
                self.i += 1;
                Ok(Expr::Str(s))
            }
            Some(Tok::Num(n)) => {
                self.i += 1;
                Ok(Expr::Num(n))
            }
            Some(Tok::Ident(name)) => {
                self.i += 1;
                let mut filter = None;
                if self.peek() == Some(&Tok::Pipe) {
                    self.i += 1;
                    match self.t.get(self.i).cloned() {
                        Some(Tok::Ident(f)) => {
                            self.i += 1;
                            filter = Some(f);
                        }
                        _ => return Err("filter name expected".into()),
                    }
                }
                Ok(Expr::Var(name, filter))
            }
            other => Err(format!("unexpected {other:?}")),
        }
    }
}

fn parse(s: &str) -> Result<Expr, String> {
    let mut p = P { t: lex(s)?, i: 0 };
    let e = p.or()?;
    if p.i != p.t.len() {
        return Err("trailing tokens".into());
    }
    Ok(e)
}

/// What the emitter gets for a list of `when` strings.
#[derive(Debug, Default)]
pub struct Analysis {
    /// Registers tested `.rc != 0`, to fold into `unless =`.
    pub rc_ne0: Vec<String>,
    /// Rust boolean expressions, all of which must hold.
    pub exprs: Vec<String>,
    /// Conditions that could not be translated, `cond (why)`.
    pub bad: Vec<String>,
    /// Flags used that no image reaches.
    pub unreachable: Vec<String>,
}

pub fn analyze(conds: &[String]) -> Analysis {
    let mut a = Analysis::default();
    for c in conds {
        let c = c.trim();
        let expr = match parse(c) {
            Ok(e) => e,
            Err(why) => {
                a.bad.push(format!("{c} ({why})"));
                continue;
            }
        };
        let conj = match expr {
            Expr::And(v) => v,
            e => vec![e],
        };
        let mut whys: Vec<String> = Vec::new();
        for e in conj {
            if let Expr::Cmp(l, "!=", r) = &e
                && let (Expr::Var(n, None), Expr::Num(0)) = (&**l, &**r)
                && let Some(reg) = n.strip_suffix(".rc")
            {
                a.rc_ne0.push(reg.to_string());
                continue;
            }
            match rust(&e, &mut a.unreachable) {
                Ok(r) => a.exprs.push(r),
                Err(why) => {
                    if !whys.contains(&why) {
                        whys.push(why);
                    }
                }
            }
        }
        if !whys.is_empty() {
            a.bad.push(format!("{c} ({})", whys.join("; ")));
        }
    }
    a
}

fn distro(name: &str) -> Option<String> {
    Some(match name {
        "Fedora" => "Distro::Fedora".into(),
        "Rocky" => "Distro::Rocky".into(),
        "AlmaLinux" => "Distro::Alma".into(),
        "Ubuntu" => "Distro::Ubuntu".into(),
        "Debian" => "Distro::Debian".into(),
        "Amazon" => "Distro::Amazon".into(),
        "Pop!_OS" => "Distro::PopOs".into(),
        "CentOS" => "Distro::Other(\"centos\".to_string())".into(),
        "RedHat" => "Distro::Other(\"rhel\".to_string())".into(),
        _ => return None,
    })
}

fn rust(e: &Expr, unreachable: &mut Vec<String>) -> Result<String, String> {
    match e {
        Expr::Or(v) => Ok(format!(
            "({})",
            v.iter()
                .map(|x| rust(x, unreachable))
                .collect::<Result<Vec<_>, _>>()?
                .join(" || ")
        )),
        Expr::And(v) => Ok(format!(
            "({})",
            v.iter()
                .map(|x| rust(x, unreachable))
                .collect::<Result<Vec<_>, _>>()?
                .join(" && ")
        )),
        Expr::Not(x) => Ok(format!("!({})", rust(x, unreachable)?)),
        Expr::Var(n, f) if matches!(f.as_deref(), None | Some("bool")) => {
            if n == "true" || n == "True" {
                return Ok("true".into());
            }
            if n == "false" || n == "False" {
                return Ok("false".into());
            }
            match flags::find(n) {
                Some(fl) => {
                    if fl.unreachable && !unreachable.iter().any(|u| u == n) {
                        unreachable.push(n.clone());
                    }
                    Ok(format!("{n}(&h.os)"))
                }
                None if n.ends_with(".rc") => {
                    Err(format!("register result `{n}` is only known at run time"))
                }
                None => Err(format!("variable `{n}` is not a distro flag")),
            }
        }
        Expr::Cmp(l, op, r) => match (&**l, &**r) {
            (Expr::Var(n, None), Expr::Str(s))
                if n == "ansible_distribution" && matches!(*op, "==" | "!=") =>
            {
                let d = distro(s).ok_or_else(|| format!("unknown distribution `{s}`"))?;
                Ok(format!("h.os.distro {op} {d}"))
            }
            (Expr::Var(n, Some(f)), Expr::Num(k))
                if n == "ansible_distribution_major_version" && f == "int" =>
            {
                Ok(format!("h.os.version.major().is_some_and(|v| v {op} {k})"))
            }
            _ => Err("comparison not understood".into()),
        },
        Expr::In(l, r) => match (&**l, &**r) {
            (Expr::Var(n, None), Expr::List(items)) if n == "ansible_distribution" => {
                let mut parts = Vec::new();
                for it in items {
                    let Expr::Str(s) = it else {
                        return Err("list item not a string".into());
                    };
                    let d = distro(s).ok_or_else(|| format!("unknown distribution `{s}`"))?;
                    parts.push(format!("h.os.distro == {d}"));
                }
                Ok(format!("({})", parts.join(" || ")))
            }
            _ => Err("`in` test not understood".into()),
        },
        Expr::Var(n, f) => Err(format!(
            "filter `{n}|{}` not understood",
            f.as_deref().unwrap_or_default()
        )),
        Expr::Str(_) | Expr::Num(_) | Expr::List(_) => Err("bare literal".into()),
    }
}

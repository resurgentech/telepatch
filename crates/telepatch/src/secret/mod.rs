//! A secret the role holds and the store resolves.
//!
//! `Display` and `Debug` are `<secret>`. The bytes live in [`SecretValue`],
//! which is wiped on drop and has no `Debug`. Built-in backends are `env`,
//! `file` and `prompt`. Anything else is a [`SecretBackend`] registered with
//! [`crate::cli::Main::secrets`]. `plan` does not resolve; a real run does,
//! in prepare, before that window changes the host.

mod env;
mod file;
mod prompt;
mod value;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt;

use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::Zeroize;

pub use env::Env;
pub use file::File;
pub use prompt::{Prompt, read_line};

use crate::engine::WindowOp;
use crate::trace::OpSpec;

thread_local! {
    static PATH: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static LITERALS: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    static TRACK: Cell<bool> = const { Cell::new(false) };
    static STORE: RefCell<Store> = RefCell::new(Store::default());
}

#[derive(Default)]
struct Store {
    extra: Vec<Box<dyn SecretBackend>>,
    /// `(scheme, path, field)` -> the backend's result, without the var or role.
    cache: HashMap<(String, String, String), Result<SecretValue, String>>,
}

/// Drop guard: pop one path segment.
pub(crate) struct PathGuard;

impl Drop for PathGuard {
    fn drop(&mut self) {
        PATH.with(|p| {
            p.borrow_mut().pop();
        });
    }
}

pub(crate) fn push_path(key: &str) -> PathGuard {
    PATH.with(|p| p.borrow_mut().push(key.to_string()));
    PathGuard
}

fn current_path() -> String {
    PATH.with(|p| p.borrow().join("."))
}

struct TrackGuard(bool);

impl Drop for TrackGuard {
    fn drop(&mut self) {
        TRACK.with(|t| t.set(self.0));
    }
}

fn track_paths() -> TrackGuard {
    TrackGuard(TRACK.with(|t| {
        let prev = t.get();
        t.set(true);
        prev
    }))
}

fn tracking() -> bool {
    TRACK.with(|t| t.get())
}

fn note_literal(path: &str) {
    if !tracking() {
        return;
    }
    let path = if path.is_empty() {
        "(unknown)".to_string()
    } else {
        path.to_string()
    };
    LITERALS.with(|l| {
        if let Some(v) = l.borrow_mut().as_mut()
            && !v.iter().any(|e| e == &path)
        {
            v.push(path);
        }
    });
}

struct LiteralGuard(Option<Vec<String>>);

impl Drop for LiteralGuard {
    fn drop(&mut self) {
        LITERALS.with(|l| *l.borrow_mut() = self.0.take());
    }
}

/// Run `f` and return the dotted paths of `Secret` fields that were literals.
pub(crate) fn collect_literals<R>(f: impl FnOnce() -> R) -> (R, Vec<String>) {
    let prev = LITERALS.with(|l| l.replace(Some(Vec::new())));
    let guard = LiteralGuard(prev);
    let r = f();
    let hits = LITERALS.with(|l| l.borrow_mut().take().unwrap_or_default());
    drop(guard);
    (r, hits)
}

/// Clear the per-run read cache. Backends stay. Called at the start of each run.
pub(crate) fn begin_run() {
    STORE.with(|s| s.borrow_mut().cache.clear());
}

struct BackendGuard(Vec<Box<dyn SecretBackend>>);

impl Drop for BackendGuard {
    fn drop(&mut self) {
        STORE.with(|s| s.borrow_mut().extra = std::mem::take(&mut self.0));
    }
}

/// Install `backends` for the duration of `f`. Built-ins stay in front.
pub fn with_backends<R>(backends: Vec<Box<dyn SecretBackend>>, f: impl FnOnce() -> R) -> R {
    let prev = STORE.with(|s| std::mem::replace(&mut s.borrow_mut().extra, backends));
    let _guard = BackendGuard(prev);
    f()
}

pub(crate) fn from_value<T: serde::de::DeserializeOwned>(value: &toml::Value) -> Result<T, String> {
    value::from_value(value)
}

/// The bytes of a secret. Wiped on drop. No `Debug`.
pub struct SecretValue {
    inner: String,
}

impl SecretValue {
    pub fn new(value: impl Into<String>) -> SecretValue {
        SecretValue {
            inner: value.into(),
        }
    }

    pub(crate) fn expose(&self) -> &str {
        &self.inner
    }
}

impl Clone for SecretValue {
    fn clone(&self) -> Self {
        SecretValue {
            inner: self.inner.clone(),
        }
    }
}

impl PartialEq for SecretValue {
    fn eq(&self, other: &Self) -> bool {
        self.inner == other.inner
    }
}

impl Eq for SecretValue {}

impl Drop for SecretValue {
    fn drop(&mut self) {
        self.inner.zeroize();
    }
}

/// Where a secret lives. `var` is the dotted field (`admin_users.default_password`).
/// `role` is filled when the trace is resolved.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretRef {
    pub scheme: String,
    pub path: String,
    pub field: String,
    pub var: String,
    pub role: String,
}

impl fmt::Debug for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<secret>")
    }
}

/// A backend error. The message names a path or a field, never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretError {
    message: String,
}

impl SecretError {
    pub fn new(message: impl Into<String>) -> SecretError {
        SecretError {
            message: message.into(),
        }
    }
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SecretError {}

/// A store the playbook binary registers. `env`, `file` and `prompt` are built in.
pub trait SecretBackend {
    fn scheme(&self) -> &'static str;
    fn resolve(&self, refs: &[SecretRef]) -> Vec<Result<SecretValue, SecretError>>;
}

#[derive(Clone)]
enum Kind {
    Literal { value: SecretValue, var: String },
    Ref(SecretRef),
    Resolved { value: SecretValue, from: SecretRef },
}

/// A password hash or other secret: a literal or a reference. Prints `<secret>`.
#[derive(Clone)]
pub struct Secret {
    kind: Kind,
}

impl Secret {
    pub fn literal(value: impl Into<String>) -> Secret {
        Secret::literal_at(value, "")
    }

    fn literal_at(value: impl Into<String>, var: &str) -> Secret {
        Secret {
            kind: Kind::Literal {
                value: SecretValue::new(value),
                var: var.to_string(),
            },
        }
    }

    pub fn reference(
        scheme: impl Into<String>,
        path: impl Into<String>,
        field: Option<&str>,
        var: impl Into<String>,
    ) -> Secret {
        Secret {
            kind: Kind::Ref(SecretRef {
                scheme: scheme.into(),
                path: path.into(),
                field: field.unwrap_or("").to_string(),
                var: var.into(),
                role: String::new(),
            }),
        }
    }

    /// The resolved or literal bytes. `None` for a reference not yet resolved.
    pub(crate) fn plain(&self) -> Option<&str> {
        match &self.kind {
            Kind::Literal { value, .. } | Kind::Resolved { value, .. } => Some(value.expose()),
            Kind::Ref(_) => None,
        }
    }

    pub(crate) fn var_name(&self) -> &str {
        match &self.kind {
            Kind::Literal { var, .. } => var,
            Kind::Ref(r) | Kind::Resolved { from: r, .. } => &r.var,
        }
    }

    fn unresolved(&self) -> Option<&SecretRef> {
        match &self.kind {
            Kind::Ref(r) => Some(r),
            _ => None,
        }
    }

    fn resolve_with(&mut self, value: SecretValue) {
        if let Kind::Ref(r) = &self.kind {
            let from = r.clone();
            self.kind = Kind::Resolved { value, from };
        }
    }
}

impl PartialEq for Secret {
    fn eq(&self, other: &Self) -> bool {
        fn key(r: &SecretRef) -> (&str, &str, &str) {
            (&r.scheme, &r.path, &r.field)
        }
        match (&self.kind, &other.kind) {
            (Kind::Literal { value: a, .. }, Kind::Literal { value: b, .. }) => a == b,
            (Kind::Ref(a), Kind::Ref(b)) => key(a) == key(b),
            (
                Kind::Resolved {
                    value: av,
                    from: ar,
                },
                Kind::Resolved {
                    value: bv,
                    from: br,
                },
            ) => av == bv && key(ar) == key(br),
            (Kind::Literal { value: a, .. }, Kind::Resolved { value: b, .. })
            | (Kind::Resolved { value: a, .. }, Kind::Literal { value: b, .. }) => a == b,
            (Kind::Ref(a), Kind::Resolved { from: b, .. })
            | (Kind::Resolved { from: a, .. }, Kind::Ref(b)) => key(a) == key(b),
            _ => false,
        }
    }
}

impl Eq for Secret {}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<secret>")
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<secret>")
    }
}

impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.kind {
            Kind::Literal { .. } => Err(serde::ser::Error::custom(
                "a secret literal is not serialized",
            )),
            Kind::Ref(r) | Kind::Resolved { from: r, .. } => {
                let n = 1 + usize::from(r.scheme == "bao");
                let mut map = serializer.serialize_map(Some(n))?;
                map.serialize_entry(&r.scheme, &r.path)?;
                if r.scheme == "bao" {
                    map.serialize_entry("field", &r.field)?;
                }
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Secret, D::Error> {
        deserializer.deserialize_any(SecretVisitor)
    }
}

struct SecretVisitor;

impl<'de> Visitor<'de> for SecretVisitor {
    type Value = Secret;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a secret literal or a reference table")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Secret, E> {
        let path = current_path();
        note_literal(&path);
        Ok(Secret::literal_at(v, &path))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Secret, E> {
        self.visit_str(&v)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Secret, A::Error> {
        let mut scheme: Option<(String, String)> = None;
        let mut field: Option<String> = None;
        while let Some(k) = map.next_key::<String>()? {
            let v: String = map.next_value()?;
            match k.as_str() {
                "field" => field = Some(v),
                "bao" | "env" | "file" | "prompt" => {
                    if scheme.is_some() {
                        return Err(de::Error::custom("a secret reference has one scheme"));
                    }
                    scheme = Some((k, v));
                }
                other => {
                    return Err(de::Error::custom(format!("unknown secret key `{other}`")));
                }
            }
        }
        let Some((scheme, path)) = scheme else {
            return Err(de::Error::custom("a secret reference needs a scheme"));
        };
        let field = if scheme == "bao" {
            Some(field.ok_or_else(|| de::Error::custom("a bao reference needs a field"))?)
        } else if field.is_some() {
            return Err(de::Error::custom(format!(
                "`{scheme}` reference has no field"
            )));
        } else {
            None
        };
        Ok(Secret::reference(
            scheme,
            path,
            field.as_deref(),
            current_path(),
        ))
    }
}

fn var_label(secret: &Secret, r: &SecretRef) -> String {
    let v = secret.var_name();
    if v.is_empty() {
        format!("{}:{}", r.scheme, r.path)
    } else {
        v.to_string()
    }
}

fn role_label(role: &str) -> &str {
    if role.is_empty() { "(none)" } else { role }
}

fn cache_key(r: &SecretRef) -> (String, String, String) {
    (r.scheme.clone(), r.path.clone(), r.field.clone())
}

fn call_scheme(scheme: &str, refs: &[SecretRef]) -> Vec<Result<SecretValue, SecretError>> {
    match scheme {
        "env" => return Env.resolve(refs),
        "file" => return File.resolve(refs),
        "prompt" => return Prompt.resolve(refs),
        _ => {}
    }
    let pos = STORE.with(|s| s.borrow().extra.iter().position(|b| b.scheme() == scheme));
    let Some(pos) = pos else {
        return refs
            .iter()
            .map(|_| SecretError::new(format!("unknown scheme `{scheme}`")))
            .map(Err)
            .collect();
    };
    let backend = STORE.with(|s| s.borrow_mut().extra.remove(pos));
    let results = backend.resolve(refs);
    STORE.with(|s| {
        let mut st = s.borrow_mut();
        let at = pos.min(st.extra.len());
        st.extra.insert(at, backend);
    });
    results
}

/// Resolve every secret reference in `ops`. One backend call per scheme for
/// the refs not already read in this run; backends read each path once per call.
pub(crate) fn resolve_ops(ops: &mut [WindowOp]) -> Result<(), String> {
    let mut jobs: Vec<(usize, String, SecretRef)> = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        let Some(secret) = user_secret(&op.node.spec) else {
            continue;
        };
        let Some(r) = secret.unresolved().cloned() else {
            continue;
        };
        let role = op
            .members
            .first()
            .map(|m| m.role.clone())
            .unwrap_or_default();
        jobs.push((i, role, r));
    }
    if jobs.is_empty() {
        return Ok(());
    }

    let mut raw: Vec<Option<Result<SecretValue, String>>> = vec![None; jobs.len()];
    let mut missing: HashMap<String, Vec<usize>> = HashMap::new();
    for (n, (_, _, r)) in jobs.iter().enumerate() {
        let key = cache_key(r);
        let hit = STORE.with(|s| s.borrow().cache.get(&key).cloned());
        if let Some(hit) = hit {
            raw[n] = Some(hit);
        } else {
            missing.entry(r.scheme.clone()).or_default().push(n);
        }
    }
    for (scheme, idxs) in missing {
        let batch: Vec<SecretRef> = idxs.iter().map(|n| jobs[*n].2.clone()).collect();
        let got = call_scheme(&scheme, &batch);
        if got.len() != batch.len() {
            return Err(format!(
                "secret: backend `{scheme}` returned {} results for {} references",
                got.len(),
                batch.len()
            ));
        }
        for (n, res) in idxs.into_iter().zip(got) {
            let stored = res.map_err(|e| e.to_string());
            let key = cache_key(&jobs[n].2);
            STORE.with(|s| s.borrow_mut().cache.insert(key, stored.clone()));
            raw[n] = Some(stored);
        }
    }

    for (n, (i, role, r)) in jobs.iter().enumerate() {
        match raw[n].take().unwrap() {
            Err(msg) => {
                let secret = user_secret(&ops[*i].node.spec).unwrap();
                let var = var_label(secret, r);
                return Err(format!("secret {var}: {msg} (role {})", role_label(role)));
            }
            Ok(value) => {
                if value.expose().contains(['\n', '\r', '\0']) {
                    let secret = user_secret(&ops[*i].node.spec).unwrap();
                    let var = var_label(secret, r);
                    return Err(format!(
                        "secret {var}: password_hash must be a single line (role {})",
                        role_label(role)
                    ));
                }
                let secret = user_secret_mut(&mut ops[*i].node.spec).unwrap();
                secret.resolve_with(value);
            }
        }
    }
    Ok(())
}

fn user_secret(spec: &OpSpec) -> Option<&Secret> {
    match spec {
        OpSpec::User(u) => u.password_secret(),
        _ => None,
    }
}

fn user_secret_mut(spec: &mut OpSpec) -> Option<&mut Secret> {
    match spec {
        OpSpec::User(u) => u.password_secret_mut(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_trims_one_trailing_newline_and_reads_a_path_once() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s");
        std::fs::write(&p, "hello\n\n").unwrap();
        let r = SecretRef {
            scheme: "file".into(),
            path: p.display().to_string(),
            field: String::new(),
            var: "pw".into(),
            role: String::new(),
        };
        let v = File.resolve(&[r.clone(), r]);
        assert_eq!(v.len(), 2);
        let (Ok(a), Ok(b)) = (&v[0], &v[1]) else {
            panic!("file backend failed");
        };
        assert_eq!(a.expose(), "hello\n");
        assert_eq!(b.expose(), a.expose());
    }

    #[test]
    fn prompt_reads_one_line_without_the_newline() {
        let got = read_line("pw? ", &b"line\n"[..]).unwrap();
        assert_eq!(got, "line");
    }

    #[test]
    fn env_miss_names_the_variable() {
        let r = SecretRef {
            scheme: "env".into(),
            path: "TP_T88_NO_SUCH_ENV_VAR".into(),
            field: String::new(),
            var: "pw".into(),
            role: String::new(),
        };
        let Err(err) = Env.resolve(&[r]).pop().unwrap() else {
            panic!("env miss returned a value");
        };
        assert!(err.to_string().contains("TP_T88_NO_SUCH_ENV_VAR"));
        assert!(!err.to_string().contains("keep"));
    }
}

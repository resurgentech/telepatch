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
    static RENDER: RefCell<RenderState> = const { RefCell::new(RenderState::new()) };
    static CAPTURED: RefCell<Vec<Secret>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone)]
struct RenderState {
    reveal: bool,
    capture: bool,
    unresolved: bool,
    touched: u32,
}

impl RenderState {
    const fn new() -> RenderState {
        RenderState {
            reveal: false,
            capture: false,
            unresolved: false,
            touched: 0,
        }
    }
}

/// What a template render observed about [`Secret`] formatting.
pub(crate) struct RenderOut {
    pub secret: bool,
    pub unresolved: bool,
    pub secrets: Vec<Secret>,
}

struct RenderGuard {
    prev: RenderState,
    prev_cap: Option<Vec<Secret>>,
}

impl Drop for RenderGuard {
    fn drop(&mut self) {
        RENDER.with(|st| *st.borrow_mut() = self.prev.clone());
        if let Some(cap) = self.prev_cap.take() {
            CAPTURED.with(|c| *c.borrow_mut() = cap);
        }
    }
}

/// Run `f` while [`Secret`]'s `Display` either prints `<secret>` or, when
/// `reveal` is set, the resolved value. `capture` records every secret the
/// render formatted. State is restored afterwards, including on panic.
pub(crate) fn with_render<R>(reveal: bool, capture: bool, f: impl FnOnce() -> R) -> (R, RenderOut) {
    let prev = RENDER.with(|st| {
        let mut cur = st.borrow_mut();
        let prev = cur.clone();
        *cur = RenderState {
            reveal,
            capture,
            unresolved: false,
            touched: 0,
        };
        prev
    });
    let prev_cap = if capture {
        Some(CAPTURED.with(|c| std::mem::take(&mut *c.borrow_mut())))
    } else {
        None
    };
    let guard = RenderGuard { prev, prev_cap };
    let result = f();
    let out = RENDER.with(|st| {
        let st = st.borrow();
        RenderOut {
            secret: st.touched > 0,
            unresolved: st.unresolved,
            secrets: if capture {
                CAPTURED.with(|c| std::mem::take(&mut *c.borrow_mut()))
            } else {
                Vec::new()
            },
        }
    });
    drop(guard);
    (result, out)
}

#[derive(Default)]
struct Store {
    extra: Vec<Box<dyn SecretBackend>>,
    /// `(scheme, path, field)` -> the backend's result, without the var or role.
    cache: HashMap<(String, String, String), Result<SecretValue, SecretError>>,
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
///
/// `bare` errors are already the whole prepare line (a missing token, an
/// expired token). Anything else is wrapped with the var and the role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretError {
    message: String,
    bare: bool,
}

impl SecretError {
    pub fn new(message: impl Into<String>) -> SecretError {
        SecretError {
            message: message.into(),
            bare: false,
        }
    }

    /// A prepare line that is not tied to one var.
    pub fn bare(message: impl Into<String>) -> SecretError {
        SecretError {
            message: message.into(),
            bare: true,
        }
    }

    pub fn is_bare(&self) -> bool {
        self.bare
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

    /// Lines prepare prints before any host change. Empty for the built-ins.
    fn warnings(&self) -> Vec<String> {
        Vec::new()
    }
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

    /// Literal, resolved, or a reference already in the prepare cache.
    fn revealed(&self) -> Option<String> {
        if let Some(v) = self.plain() {
            return Some(v.to_string());
        }
        let r = self.unresolved()?;
        let key = cache_key(r);
        STORE.with(|s| {
            s.borrow()
                .cache
                .get(&key)
                .and_then(|res| res.as_ref().ok().map(|v| v.expose().to_string()))
        })
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
        // Outside a template render this stays `<secret>`. Inside one,
        // [`with_render`] decides whether the value is printed.
        let (reveal, capture) = RENDER.with(|st| {
            let st = st.borrow();
            (st.reveal, st.capture)
        });
        RENDER.with(|st| {
            let mut st = st.borrow_mut();
            st.touched = st.touched.saturating_add(1);
        });
        if capture {
            CAPTURED.with(|c| c.borrow_mut().push(self.clone()));
        }
        if reveal {
            if let Some(v) = self.revealed() {
                return f.write_str(&v);
            }
            RENDER.with(|st| st.borrow_mut().unresolved = true);
        }
        f.write_str("<secret>")
    }
}

impl askama::FastWritable for Secret {
    fn write_into(
        &self,
        dest: &mut dyn std::fmt::Write,
        _values: &dyn askama::Values,
    ) -> askama::Result<()> {
        dest.write_fmt(format_args!("{self}"))?;
        Ok(())
    }
}

/// A secret's text is the value the file should hold, not an escaped form.
impl askama::filters::HtmlSafe for Secret {}

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

enum JobWhere {
    /// A user password: applied onto the op, and rejected when it is not one line.
    User,
    /// A template field: the value stays in the cache and is read at render.
    Template,
}

struct Job {
    index: usize,
    role: String,
    secret: Secret,
    reference: SecretRef,
    where_: JobWhere,
}

/// Resolve every secret reference in `ops`. One backend call per scheme for
/// the refs not already read in this run; backends read each path once per call.
/// User passwords are written back onto the op. Template secrets stay
/// references; [`Secret`]'s render reads them from the cache.
pub(crate) fn resolve_ops(ops: &mut [WindowOp]) -> Result<(), String> {
    let mut jobs: Vec<Job> = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        let role = op
            .members
            .first()
            .map(|m| m.role.clone())
            .unwrap_or_default();
        if let Some(secret) = user_secret(&op.node.spec)
            && let Some(r) = secret.unresolved().cloned()
        {
            jobs.push(Job {
                index: i,
                role: role.clone(),
                secret: secret.clone(),
                reference: r,
                where_: JobWhere::User,
            });
        }
        for secret in template_secrets(&op.node.spec) {
            let Some(r) = secret.unresolved().cloned() else {
                continue;
            };
            jobs.push(Job {
                index: i,
                role: role.clone(),
                secret,
                reference: r,
                where_: JobWhere::Template,
            });
        }
    }
    if jobs.is_empty() {
        return Ok(());
    }

    let mut raw: Vec<Option<Result<SecretValue, SecretError>>> = vec![None; jobs.len()];
    let mut missing: HashMap<String, Vec<usize>> = HashMap::new();
    for (n, job) in jobs.iter().enumerate() {
        let key = cache_key(&job.reference);
        let hit = STORE.with(|s| s.borrow().cache.get(&key).cloned());
        if let Some(hit) = hit {
            raw[n] = Some(hit);
        } else {
            missing
                .entry(job.reference.scheme.clone())
                .or_default()
                .push(n);
        }
    }
    for (scheme, idxs) in missing {
        let batch: Vec<SecretRef> = idxs.iter().map(|n| jobs[*n].reference.clone()).collect();
        let got = call_scheme(&scheme, &batch);
        if got.len() != batch.len() {
            return Err(format!(
                "secret: backend `{scheme}` returned {} results for {} references",
                got.len(),
                batch.len()
            ));
        }
        for (n, res) in idxs.into_iter().zip(got) {
            let stored = res;
            let key = cache_key(&jobs[n].reference);
            STORE.with(|s| s.borrow_mut().cache.insert(key, stored.clone()));
            raw[n] = Some(stored);
        }
    }

    for (n, job) in jobs.iter().enumerate() {
        match raw[n].take().unwrap() {
            Err(err) => {
                let var = var_label(&job.secret, &job.reference);
                let role = role_label(&job.role);
                return Err(format_backend_error(&var, role, &err));
            }
            Ok(value) => {
                if matches!(job.where_, JobWhere::User) {
                    if value.expose().contains(['\n', '\r', '\0']) {
                        let var = var_label(&job.secret, &job.reference);
                        return Err(format!(
                            "secret {var}: password_hash must be a single line (role {})",
                            role_label(&job.role)
                        ));
                    }
                    let secret = user_secret_mut(&mut ops[job.index].node.spec).unwrap();
                    secret.resolve_with(value);
                }
            }
        }
    }
    Ok(())
}

fn format_backend_error(var: &str, role: &str, err: &SecretError) -> String {
    if err.is_bare() {
        err.to_string()
    } else {
        format!("secret {var}: {err} (role {role})")
    }
}

/// Warning lines from the backends registered for this run.
pub(crate) fn backend_warnings() -> Vec<String> {
    STORE.with(|s| s.borrow().extra.iter().flat_map(|b| b.warnings()).collect())
}

fn template_secrets(spec: &OpSpec) -> Vec<Secret> {
    let content = match spec {
        OpSpec::File(f) => f.content.as_ref(),
        OpSpec::Unit(u) => u.content.as_ref(),
        _ => None,
    };
    content.map(|c| c.secret_refs()).unwrap_or_default()
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
    fn bare_backend_error_is_the_whole_line() {
        let bare = SecretError::bare("openbao: no token: set BAO_TOKEN or VAULT_TOKEN");
        assert_eq!(
            format_backend_error("pw", "admin_users", &bare),
            "openbao: no token: set BAO_TOKEN or VAULT_TOKEN"
        );
        let wrapped = SecretError::new("no secret at kv/app");
        assert_eq!(
            format_backend_error("telegraf.influx_token", "telegraf", &wrapped),
            "secret telegraf.influx_token: no secret at kv/app (role telegraf)"
        );
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

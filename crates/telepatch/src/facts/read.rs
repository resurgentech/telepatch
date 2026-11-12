//! Fact futures: `h.exists(..)`, `h.bin_exists(..)`, `h.bin_version(..)`,
//! `h.user_info(..)`, `h.stat(..)`, `h.pkg_installed(..)`, `h.check(..)`.
//! On poll they ask the resolver; an unanswerable read registers itself and
//! returns `Pending` until the engine has read it.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use crate::facts::os::Family;
use crate::facts::query::{Fact, FactQuery};
use crate::host::{Host, TraceError};
use crate::key::{Check, Stat, UserInfo, Value};
use crate::resolver::{Answer, Resolver};
use crate::trace::{Node, Site, Source, Trace};

/// A pending fact read; resolves to `T`.
#[must_use = "fact reads do nothing unless awaited"]
pub struct FactFuture<T> {
    trace: Rc<RefCell<Trace>>,
    resolver: Rc<RefCell<Resolver>>,
    query: FactQuery,
    ticket: Option<u64>,
    conv: fn(Value) -> T,
    /// Where the role asked (`#[track_caller]`), for a `--check` that stops
    /// at this read.
    site: Site,
}

impl<T> Unpin for FactFuture<T> {}

impl<T> Future for FactFuture<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<T> {
        let this = &mut *self;
        let mut resolver = this.resolver.borrow_mut();
        let (value, source) = match this.ticket {
            Some(t) => match resolver.take_answer(t) {
                Some(v) => (v, Source::Block),
                None => return Poll::Pending,
            },
            None => {
                let answer = resolver.resolve(&this.trace.borrow(), &this.query);
                match answer {
                    Answer::Now(v, s) => {
                        resolver.note_resolved(&this.query, &v, &s);
                        (v, s)
                    }
                    Answer::Block => {
                        this.ticket = Some(resolver.register(this.query.clone(), this.site));
                        return Poll::Pending;
                    }
                }
            }
        };
        this.trace.borrow_mut().push(Node::FactRead {
            key: this.query.fact.effect_key(),
            value: value.clone(),
            source,
        });
        Poll::Ready((this.conv)(value))
    }
}

impl<V> Host<V> {
    #[track_caller]
    fn fact<T>(&self, fact: Fact, conv: fn(Value) -> T) -> FactFuture<T> {
        FactFuture {
            trace: self.trace_cell().clone(),
            resolver: self.resolver_cell().clone(),
            query: FactQuery {
                run_as: self.run_as().clone(),
                fact,
            },
            ticket: None,
            conv,
            site: Site::caller(),
        }
    }

    /// Does `path` exist? Reads `file:path`.
    #[track_caller]
    pub fn exists(&self, path: &str) -> FactFuture<bool> {
        self.fact(Fact::exists(path), |v| v == Value::Exists(true))
    }

    /// Is `cmd` on `PATH` in this handle's environment?
    #[track_caller]
    pub fn bin_exists(&self, cmd: &str) -> FactFuture<bool> {
        self.fact(Fact::bin_exists(cmd), |v| v == Value::Exists(true))
    }

    /// First line of `cmd --version`, or `None` when `cmd` is missing.
    #[track_caller]
    pub fn bin_version(&self, cmd: &str) -> FactFuture<Option<String>> {
        self.fact(Fact::bin_version(cmd), |v| match v {
            Value::Text(s) => Some(s),
            _ => None,
        })
    }

    /// Home directory, login shell, and whether the account exists.
    #[track_caller]
    pub fn user_info(&self, name: &str) -> FactFuture<UserInfo> {
        self.fact(Fact::user_info(name), |v| match v {
            Value::User(u) => u,
            _ => UserInfo {
                home: String::new(),
                shell: String::new(),
                exists: false,
            },
        })
    }

    /// Mode, owner, size, and whether `path` exists.
    #[track_caller]
    pub fn stat(&self, path: &str) -> FactFuture<Stat> {
        self.fact(Fact::stat(path), |v| match v {
            Value::Stat(s) => s,
            _ => Stat {
                mode: String::new(),
                owner: String::new(),
                size: 0,
                exists: false,
            },
        })
    }

    /// The installed version, or `None`. Apt, dnf and pacman only; another
    /// family is a trace error.
    #[track_caller]
    pub fn pkg_installed(&self, name: &str) -> FactFuture<Option<String>> {
        if let Family::Other(f) = &self.os.family {
            self.record_error(TraceError::new(format!(
                "pkg_installed(): no package manager known for os family {f:?}"
            )));
        }
        self.fact(Fact::pkg_installed(name), |v| match v {
            Value::Pkg(v) => v,
            _ => None,
        })
    }

    /// Run `cmd` with `bash -c` (no persistent shell changes). `rc` is its
    /// exit status; `stdout` is its standard output.
    #[track_caller]
    pub fn check(&self, cmd: &str) -> FactFuture<Check> {
        self.fact(Fact::check(cmd), |v| match v {
            Value::Check(c) => c,
            _ => Check {
                rc: 1,
                stdout: String::new(),
            },
        })
    }
}

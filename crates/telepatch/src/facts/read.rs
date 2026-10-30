//! Fact futures: `h.exists(..)`, `h.bin_exists(..)`, `h.bin_version(..)`.
//! On poll they ask the resolver; an unanswerable read registers itself and
//! returns `Pending` until the engine has read it.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use crate::facts::query::{Fact, FactQuery};
use crate::host::Host;
use crate::key::Value;
use crate::resolver::{Answer, Resolver};
use crate::trace::{Node, Source, Trace};

/// A pending fact read; resolves to `T`.
#[must_use = "fact reads do nothing unless awaited"]
pub struct FactFuture<T> {
    trace: Rc<RefCell<Trace>>,
    resolver: Rc<RefCell<Resolver>>,
    query: FactQuery,
    ticket: Option<u64>,
    conv: fn(Value) -> T,
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
                        this.ticket = Some(resolver.register(this.query.clone()));
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
        }
    }

    /// Does `path` exist? Reads `file:path`.
    pub fn exists(&self, path: &str) -> FactFuture<bool> {
        self.fact(Fact::exists(path), |v| v == Value::Exists(true))
    }

    /// Is `cmd` on `PATH` in this handle's environment?
    pub fn bin_exists(&self, cmd: &str) -> FactFuture<bool> {
        self.fact(Fact::bin_exists(cmd), |v| v == Value::Exists(true))
    }

    /// First line of `cmd --version`, or `None` when `cmd` is missing.
    pub fn bin_version(&self, cmd: &str) -> FactFuture<Option<String>> {
        self.fact(Fact::bin_version(cmd), |v| match v {
            Value::Text(s) => Some(s),
            _ => None,
        })
    }
}

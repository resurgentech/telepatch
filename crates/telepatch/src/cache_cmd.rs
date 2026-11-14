//! `cache usage` and `cache purge` over a set of hosts (T123): one root
//! shell per host, one script per host ([`crate::ops::fetch::cache_usage_script`]
//! and [`crate::ops::fetch::cache_purge_script`]) sent as a probe, its
//! output read into a [`CacheReport`]. Declared inside [`crate::executor`]
//! (`#[path]`) for its transports, shell handshake and refusal texts.
//!
//! The shell is a run's root shell with no playbook (`init_shell` with no
//! owner). `--serial n` limits the hosts in flight; the events
//! of `--log` are `host_open` and `host_status` as a run's are.

use super::*;
use crate::report::{CacheHost, CacheReport};

/// What to do on every host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheAction {
    /// Read the registry, the records and the files. No lock, no change.
    Usage,
    /// Remove the caches (`owner`: only that owner's share).
    Purge { owner: Option<String> },
}

/// Run `action` on every host (`(name, target)`) and return the report in
/// the order given. Builds its own current-thread runtime, so call it from
/// plain (non-async) code.
pub fn run_cache(
    hosts: Vec<(String, Target)>,
    action: CacheAction,
    log: Option<PathBuf>,
    serial: Option<usize>,
) -> CacheReport {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("building the tokio runtime");
    let log = log.as_ref().and_then(|p| match std::fs::File::create(p) {
        Ok(f) => Some(RefCell::new(std::io::BufWriter::new(f))),
        Err(e) => {
            eprintln!("telepatch: cannot open log {}: {e}", p.display());
            None
        }
    });
    let sink = Rc::new(Sink {
        stdout: Stdout::Quiet,
        log,
    });
    let script = Rc::new(match &action {
        CacheAction::Usage => crate::ops::fetch::cache_usage_script(),
        CacheAction::Purge { owner } => crate::ops::fetch::cache_purge_script(owner.as_deref()),
    });
    let permits = Rc::new(tokio::sync::Semaphore::new(
        serial.unwrap_or(usize::MAX >> 4),
    ));
    let names: Vec<String> = hosts.iter().map(|h| h.0.clone()).collect();
    let local = tokio::task::LocalSet::new();
    let reports =
        rt.block_on(local.run_until(async {
            let tasks: Vec<_> = hosts
                .into_iter()
                .map(|(name, target)| {
                    let (sink, script, permits) = (sink.clone(), script.clone(), permits.clone());
                    tokio::task::spawn_local(async move {
                        let _permit = permits
                            .acquire()
                            .await
                            .expect("the limiter is never closed");
                        cache_one(name, target, &script, &sink).await
                    })
                })
                .collect();
            let mut out = Vec::new();
            for (t, name) in tasks.into_iter().zip(&names) {
                out.push(t.await.unwrap_or_else(|e| {
                    CacheHost::failed(name, format!("host task panicked: {e}"))
                }));
            }
            out
        }));
    CacheReport { hosts: reports }
}

async fn cache_one(name: String, target: Target, script: &str, sink: &Sink) -> CacheHost {
    sink.event(json!({"ev":"host_open","host":name}));
    let host = cache_read(&name, &target, script).await;
    close_target(&target).await;
    let (status, detail) = match &host.status {
        crate::report::CacheStatus::Ok => ("ok", String::new()),
        crate::report::CacheStatus::Failed(w) => ("failed", w.clone()),
    };
    sink.event(json!({"ev":"host_status","host":name,"status":status,"detail":detail}));
    host
}

async fn cache_read(name: &str, target: &Target, script: &str) -> CacheHost {
    let mut shells = target.open(name, &Principal::Root, 1).await;
    if shells.got.is_empty() {
        return CacheHost::failed(name, refusal(target, name, &Principal::Root, shells.limit));
    }
    let mut shell = shells.got.remove(0);
    for extra in shells.got {
        extra.close().await;
    }
    let res = match init_shell(&mut shell, None).await {
        Ok(()) => prepare::probe(&mut shell, 1, script).await,
        Err(e) => Err(format!("opening the shell: {e}")),
    };
    shell.close().await;
    match res {
        Ok((_, out)) => CacheHost::parse(name, &out),
        Err(e) => CacheHost::failed(name, e),
    }
}

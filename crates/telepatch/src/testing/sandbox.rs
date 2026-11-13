//! The test sandbox (T124). `.cargo/config.toml` sets `TP_SANDBOX_REQUIRED=1`
//! for every process cargo runs, so every shell a test opens refuses a
//! stage, cache, run directory or filesystem root that does not lie under
//! `$TP_SANDBOX` (see [`crate::sandbox::SBX`]). A test builds a
//! [`sandbox`] and hands its environment to the transport
//! (`LocalTransport::sandboxed`, `SshTransport::sandboxed`,
//! `DockerTransport::sandboxed`); a test that checks the refusal builds a
//! bare transport on purpose.

use std::path::{Path, PathBuf};

/// A directory of the test's own and the environment that points every seam
/// path into it. It must outlive the run that uses it.
pub struct Sandbox {
    _dir: Option<tempfile::TempDir>,
    root: PathBuf,
    /// `TP_FETCH_STAGE`: `<root>/stage`.
    pub stage: PathBuf,
    /// `TP_FETCH_CACHE`: `<root>/cache`.
    pub cache: PathBuf,
    /// `TP_RUN_DIR`: `<root>/run`.
    pub run: PathBuf,
    /// `TP_FS_ROOTS`: `<root>/fs`.
    pub fs: PathBuf,
}

/// A fresh sandbox in its own temporary directory, with `stage`, `cache`,
/// `run` and `fs` created inside it.
pub fn sandbox() -> Sandbox {
    let dir = tempfile::tempdir().expect("sandbox: tempdir");
    // A canonical root, so `$TP_SANDBOX` is the path the shell sees.
    let root = dir.path().canonicalize().expect("sandbox: canonicalize");
    let sb = Sandbox::new(Some(dir), root);
    for d in [&sb.stage, &sb.cache, &sb.run, &sb.fs] {
        std::fs::create_dir_all(d).expect("sandbox: create dir");
    }
    sb
}

impl Sandbox {
    fn new(dir: Option<tempfile::TempDir>, root: PathBuf) -> Sandbox {
        Sandbox {
            stage: root.join("stage"),
            cache: root.join("cache"),
            run: root.join("run"),
            fs: root.join("fs"),
            _dir: dir,
            root,
        }
    }

    /// A sandbox in a directory the test already owns (and keeps alive):
    /// the same environment, nothing created. For a test whose fixtures
    /// live in `dir` (a stage at `dir/stage`) and that sets other seams
    /// itself; an env pair the test adds after [`Sandbox::env`] wins.
    pub fn at(dir: &Path) -> Sandbox {
        Sandbox::new(None, dir.to_path_buf())
    }

    /// The sandbox directory (`TP_SANDBOX`).
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// `TP_SANDBOX`, `TP_FETCH_STAGE`, `TP_FETCH_CACHE`, `TP_RUN_DIR` and
    /// `TP_FS_ROOTS`.
    pub fn env(&self) -> Vec<(String, String)> {
        let s = |p: &Path| p.display().to_string();
        vec![
            ("TP_SANDBOX".into(), s(&self.root)),
            ("TP_FETCH_STAGE".into(), s(&self.stage)),
            ("TP_FETCH_CACHE".into(), s(&self.cache)),
            ("TP_RUN_DIR".into(), s(&self.run)),
            ("TP_FS_ROOTS".into(), s(&self.fs)),
        ]
    }
}

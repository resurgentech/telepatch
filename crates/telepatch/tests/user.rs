//! The `user` primitive: template run against fake `getent` / `id` /
//! `useradd` / `usermod` / `chpasswd` on PATH, wrapped-script and plan
//! snapshots, the root-only trace error, the password hash never leaving
//! the executed script, and the prepare lines through the real executor.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::facts::os::Os;
use telepatch::plan::render_host;
use telepatch::report::{HostStatus, Report};
use telepatch::trailer::{OpKind, UserResult, render_op, split_stream, wrap};
use telepatch::transport::local::LocalTransport;
use telepatch::{Host, Key, Node, NodeId, Playbook, PlaybookFuture, Value};

const NONCE: &str = "t0n";

fn os() -> Os {
    Os::parse("ID=debian\nVERSION_ID=12\n").unwrap()
}

fn host() -> Host<()> {
    Host::new("t", os(), Rc::new(()))
}

/// A throwaway fixture, generated per call; never a real hash.
fn fixture_hash(tag: &str) -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    format!("$6$fx{tag}{n:x}$notreal{}{n:x}", std::process::id())
}

fn wrapped(h: &Host<()>, id: NodeId) -> String {
    match &h.snapshot().nodes[id.0] {
        Node::Op(op) => render_op(op, id.0, NONCE, &h.os),
        _ => panic!("not an op"),
    }
}

/// The wrapped script as every reader sees it (`OpSpec::script`).
fn wrapped_display(h: &Host<()>, id: NodeId) -> String {
    match &h.snapshot().nodes[id.0] {
        Node::Op(op) => wrap(OpKind::User, &op.spec.script(&h.os), id.0, NONCE),
        _ => panic!("not an op"),
    }
}

const GETENT: &str = r#"case "$1" in
group) grep -qx -- "$2" @D@/groups 2>/dev/null && echo "$2:x:1:" || exit 2;;
passwd) [ -d @D@/users/"$2" ] && echo "$2:x:1000:1000::/home/$2:$(cat @D@/users/"$2"/shell)" || exit 2;;
*) exit 3;;
esac"#;

const ID: &str = r#"[ "$1" = -nG ] || exit 64
[ -d @D@/users/"$2" ] || exit 1
cat @D@/users/"$2"/groups"#;

const USERADD: &str = r#"echo "useradd $*" >> @D@/log
shell=/bin/sh; groups=
while [ $# -gt 0 ]; do
  case "$1" in
    -m) shift;;
    -s) shell=$2; shift 2;;
    -G) groups=$(echo "$2" | tr , ' '); shift 2;;
    --) name=$2; break;;
    *) exit 64;;
  esac
done
[ -d @D@/users/"$name" ] && exit 9
mkdir -p @D@/users/"$name"
echo "$shell" > @D@/users/"$name"/shell
echo "$groups" > @D@/users/"$name"/groups"#;

const USERMOD: &str = r#"echo "usermod $*" >> @D@/log
add=
while [ $# -gt 0 ]; do
  case "$1" in
    -s) echo "$2" > @D@/users/"${@: -1}"/shell; shift 2;;
    -aG) add=$(echo "$2" | tr , ' '); shift 2;;
    --) name=$2; break;;
    *) exit 64;;
  esac
done
[ -z "$add" ] || echo "$(cat @D@/users/"$name"/groups) $add" > @D@/users/"$name"/groups"#;

// Logs its argv (never its stdin), keeps what it read as the "shadow".
const CHPASSWD: &str = r#"echo "chpasswd $*" >> @D@/log
cat >> @D@/shadow"#;

/// A `bin` dir with the fakes; state under `<d>`: `groups` (a name per
/// line), `users/<n>/{shell,groups}`, `shadow`, `log`.
fn stubs(d: &Path) -> PathBuf {
    let bin = d.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(d.join("users")).unwrap();
    let w = |name: &str, body: &str| {
        let p = bin.join(name);
        let body = body.replace("@D@", &d.display().to_string());
        std::fs::write(&p, format!("#!/bin/bash\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    };
    w("getent", GETENT);
    w("id", ID);
    w("useradd", USERADD);
    w("usermod", USERMOD);
    w("chpasswd", CHPASSWD);
    bin
}

fn add_user(d: &Path, name: &str, shell: &str, groups: &str) {
    let u = d.join("users").join(name);
    std::fs::create_dir_all(&u).unwrap();
    std::fs::write(u.join("shell"), format!("{shell}\n")).unwrap();
    std::fs::write(u.join("groups"), format!("{groups}\n")).unwrap();
}

fn set_groups(d: &Path, names: &[&str]) {
    std::fs::write(d.join("groups"), format!("{}\n", names.join("\n"))).unwrap();
}

/// (result, stderr)
fn run_script(script: &str, d: &Path, bin: &Path) -> (UserResult, String) {
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .current_dir(d)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .output()
        .unwrap();
    let stream = String::from_utf8(out.stdout).unwrap();
    let mut v = split_stream(&stream, NONCE);
    assert_eq!(v.len(), 1, "stream: {stream:?}");
    let (_, t) = v.remove(0);
    (
        t.typed::<UserResult>().unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

fn log(d: &Path) -> String {
    std::fs::read_to_string(d.join("log")).unwrap_or_default()
}

fn shadow(d: &Path) -> String {
    std::fs::read_to_string(d.join("shadow")).unwrap_or_default()
}

fn shell_of(d: &Path, n: &str) -> String {
    std::fs::read_to_string(d.join("users").join(n).join("shell"))
        .unwrap()
        .trim()
        .to_string()
}

fn groups_of(d: &Path, n: &str) -> Vec<String> {
    std::fs::read_to_string(d.join("users").join(n).join("groups"))
        .unwrap()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

fn res(rc: i32, changed: bool, created: bool) -> UserResult {
    UserResult {
        rc,
        changed,
        created,
    }
}

#[test]
fn user_create_then_rerun_changes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    set_groups(d.path(), &["sudo"]);
    let hash = fixture_hash("a");
    let h = host();
    let id = h
        .user("tpdev")
        .shell("/bin/bash")
        .groups_add(["sudo"])
        .password_hash(&hash)
        .id();
    let (r, _) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!(r, res(0, true, true));
    assert_eq!(
        log(d.path()),
        "useradd -m -s /bin/bash -G sudo -- tpdev\nchpasswd -e\n",
        "the hash is never an argument"
    );
    assert_eq!(shadow(d.path()), format!("tpdev:{hash}\n"));
    assert_eq!(shell_of(d.path(), "tpdev"), "/bin/bash");
    assert_eq!(groups_of(d.path(), "tpdev"), ["sudo"]);

    let (r, _) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!(r, res(0, false, false));
    assert_eq!(log(d.path()).lines().count(), 2, "nothing ran again");
}

#[test]
fn user_groups_are_appended_never_removed() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    set_groups(d.path(), &["a", "b", "docker", "sudo"]);
    add_user(d.path(), "bot", "/bin/bash", "bot a b");
    let h = host();
    let id = h.user("bot").groups_add(["a", "docker", "sudo"]).id();
    let (r, _) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!(r, res(0, true, false));
    assert_eq!(log(d.path()), "usermod -aG docker,sudo -- bot\n");
    let g = groups_of(d.path(), "bot");
    assert!(
        ["bot", "a", "b", "docker", "sudo"]
            .iter()
            .all(|x| g.contains(&x.to_string()))
    );
    let (r, _) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!(r, res(0, false, false));
    assert_eq!(log(d.path()).lines().count(), 1);
}

#[test]
fn user_shell_is_enforced_on_an_existing_account() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    add_user(d.path(), "bot", "/bin/sh", "bot");
    let h = host();
    let id = h.user("bot").shell("/bin/bash").id();
    let (r, _) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!(r, res(0, true, false));
    assert_eq!(log(d.path()), "usermod -s /bin/bash -- bot\n");
    assert_eq!(shell_of(d.path(), "bot"), "/bin/bash");
    let (r, _) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!(r, res(0, false, false));
    assert_eq!(log(d.path()).lines().count(), 1);
}

#[test]
fn user_password_is_applied_only_on_create() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let first = fixture_hash("one");
    let second = fixture_hash("two");
    assert_ne!(first, second);
    let h = host();
    let a = h.user("u").password_hash(&first).id();
    let b = h.user("u").password_hash(&second).id();
    let (r, _) = run_script(&wrapped(&h, a), d.path(), &bin);
    assert_eq!(r, res(0, true, true));
    let (r, _) = run_script(&wrapped(&h, b), d.path(), &bin);
    assert_eq!(r, res(0, false, false), "a different hash changes nothing");
    assert_eq!(shadow(d.path()), format!("u:{first}\n"));
    assert_eq!(log(d.path()).matches("chpasswd").count(), 1);

    // An account that already existed never gets the password.
    add_user(d.path(), "old", "/bin/sh", "old");
    let c = h.user("old").password_hash(&first).id();
    let (r, _) = run_script(&wrapped(&h, c), d.path(), &bin);
    assert_eq!(r, res(0, false, false));
    assert_eq!(log(d.path()).matches("chpasswd").count(), 1);
}

#[test]
fn user_empty_hash_is_not_applied() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let h = host();
    let id = h.user("locked").password_hash("").id();
    let (r, _) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!(r, res(0, true, true));
    assert_eq!(log(d.path()), "useradd -m -- locked\n");
    assert_eq!(shadow(d.path()), "");
}

#[test]
fn user_missing_group_fails_and_changes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    set_groups(d.path(), &["sudo"]);
    let h = host();
    let id = h.user("tpdev").groups_add(["sudo", "nosuch"]).id();
    let (r, err) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!(r, res(1, false, false));
    assert!(
        err.contains("telepatch: user tpdev: group nosuch does not exist"),
        "{err}"
    );
    assert_eq!(log(d.path()), "");
    assert!(!d.path().join("users/tpdev").exists());
    // Also for an existing account.
    add_user(d.path(), "bot", "/bin/sh", "bot");
    let id = h.user("bot").groups_add(["nosuch"]).id();
    let (r, _) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!(r.rc, 1);
    assert_eq!(log(d.path()), "");
}

#[test]
fn user_names_are_quoted() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let h = host();
    let id = h.user("we ird;x").shell("/b in/sh").id();
    let (r, _) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert!(r.created);
    assert_eq!(log(d.path()), "useradd -m -s /b in/sh -- we ird;x\n");
}

#[test]
fn user_hash_is_in_the_executed_script_only() {
    let hash = fixture_hash("h");
    let h = host();
    let id = h.user("tpdev").password_hash(&hash).id();
    assert!(wrapped(&h, id).contains(&hash), "the shell gets it");
    assert!(!wrapped_display(&h, id).contains(&hash));
    assert!(wrapped_display(&h, id).contains("<hash>"));
    let plan = render_host("h", &h.os, &h.snapshot());
    assert!(!plan.contains(&hash), "{plan}");
    assert!(plan.contains("<hash>"), "{plan}");
    assert!(!format!("{:?}", h.snapshot()).contains(&hash));
}

#[test]
fn user_password_hash_must_be_one_line() {
    let h = host();
    h.user("u").password_hash("a\nb");
    let errs = h.snapshot().errors;
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(errs[0].to_string().contains("password_hash"));
}

#[test]
fn user_template_snapshots() {
    let h = host();
    let a = h.user("tpdev").id();
    let b = h
        .user("tpdev")
        .shell("/bin/bash")
        .groups_add(["sudo", "docker"])
        .id();
    let c = h
        .user("tpdev")
        .shell("/bin/bash")
        .groups_add(["sudo"])
        .password_hash("$6$fixture$placeholder")
        .id();
    insta::assert_snapshot!(format!(
        "{}\n{}\n{}",
        wrapped_display(&h, a),
        wrapped_display(&h, b),
        wrapped_display(&h, c)
    ));
}

#[test]
fn user_plan_shows_effects() {
    let h = host();
    h.group("docker");
    h.user("tpdev")
        .shell("/bin/bash")
        .groups_add(["docker"])
        .password_hash("$6$fixture$placeholder");
    h.user("bare");
    insta::assert_snapshot!(render_host("h", &h.os, &h.snapshot()));
}

#[test]
fn user_effects_are_the_user_key() {
    let h = host();
    let u = h.user("tpdev");
    u.groups_add(["docker", "sudo"]);
    let snap = h.snapshot();
    let Node::Op(op) = &snap.nodes[u.id().0] else {
        panic!()
    };
    assert_eq!(op.spec.kind(), "user");
    assert_eq!(op.writes, [Key::user("tpdev")].into());
    assert_eq!(op.reads, [Key::group("docker"), Key::group("sudo")].into());
    assert_eq!(op.post, [(Key::user("tpdev"), Value::Exists(true))].into());
    assert!(snap.errors.is_empty());
}

#[test]
fn user_is_root_only() {
    let h = host();
    h.as_user("deploy").user("tpdev");
    let errs = h.snapshot().errors;
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(
        errs[0].to_string().contains("user") && errs[0].to_string().contains("root"),
        "{}",
        errs[0]
    );
    let ok = host();
    ok.as_user("root").user("tpdev");
    ok.user("tpdev");
    assert!(ok.snapshot().errors.is_empty());
}

// --- through the executor ---

fn go(pb: &Playbook<PathBuf>, d: &Path, log_path: Option<PathBuf>) -> (Report, String) {
    let bin = stubs(d);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let out = Rc::new(RefCell::new(String::new()));
    let report = run(
        pb,
        vec![HostSpec {
            name: "h1".into(),
            target: Target::Local(LocalTransport::new().uid(0).env("PATH", &path)),
            vars: d.to_path_buf(),
        }],
        RunOpts {
            log: log_path,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    let text = out.borrow().clone();
    (report, text)
}

fn pb_for(run_: fn(Host<PathBuf>) -> PlaybookFuture) -> Playbook<PathBuf> {
    Playbook::new("user", run_)
}

/// `h.vars` is the state dir; the hash is read from its `hash` file so
/// the playbook (a plain fn) can carry a per-test fixture.
fn hash_in(h: &Host<PathBuf>) -> String {
    std::fs::read_to_string(h.vars.join("hash")).unwrap()
}

#[test]
fn user_executor_prepare_lines_then_changed_0_and_no_hash_leak() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            let hash = hash_in(&h);
            h.user("tpdev")
                .shell("/bin/bash")
                .groups_add(["sudo"])
                .password_hash(&hash);
            h.user("bot").shell("/bin/bash").groups_add(["docker"]);
            h.user("plain");
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    stubs(d.path());
    set_groups(d.path(), &["sudo", "docker"]);
    add_user(d.path(), "bot", "/bin/sh", "bot");
    let hash = fixture_hash("x");
    std::fs::write(d.path().join("hash"), &hash).unwrap();
    let pb = pb_for(run_);
    let events = d.path().join("events.jsonl");
    let (r1, out1) = go(&pb, d.path(), Some(events.clone()));
    assert_eq!(
        r1.hosts[0].status,
        HostStatus::Changed,
        "{}\n{out1}",
        r1.render()
    );
    for want in [
        "== h1 [prepare] user tpdev: will create ==",
        "== h1 [prepare] user bot: groups +docker ==",
        "== h1 [prepare] user bot: shell -> /bin/bash ==",
        "== h1 [prepare] user plain: will create ==",
    ] {
        assert!(out1.contains(want), "{want}\n{out1}");
    }
    assert!(r1.render().contains("h1: user tpdev: will create"));
    assert!(r1.render().contains("h1: user bot: groups +docker"));
    assert!(out1.contains("user tpdev"), "executor label: {out1}");
    assert_eq!(shell_of(d.path(), "bot"), "/bin/bash");
    assert_eq!(shadow(d.path()), format!("tpdev:{hash}\n"));

    // The hash is nowhere a reader looks.
    let ev = std::fs::read_to_string(&events).unwrap();
    assert!(ev.contains("\"will_create\""), "{ev}");
    for (what, text) in [
        ("stdout", &out1),
        ("report", &r1.render()),
        ("events", &ev),
        ("op descs", &format!("{:?}", r1.hosts[0].ops)),
    ] {
        assert!(!text.contains(&hash), "hash leaked into {what}:\n{text}");
    }

    let (r2, out2) = go(&pb, d.path(), None);
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());
    assert!(r2.hosts[0].ops.iter().all(|o| !o.changed));
    assert!(r2.hosts[0].will_create.is_empty());
    assert!(!out2.contains("[prepare]"), "{out2}");
    assert_eq!(log(d.path()).matches("chpasswd").count(), 1);
}

#[test]
fn user_after_group_op_in_one_run_works() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.group("tpg");
            h.user("tpdev").groups_add(["tpg"]);
            Ok(())
        })
    }
    // The group stub has no state change path for `groupadd`, so give it.
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let p = bin.join("groupadd");
    std::fs::write(
        &p,
        format!(
            "#!/bin/bash\necho \"${{@: -1}}\" >> {}/groups\n",
            d.path().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let (r, out) = go(&pb_for(run_), d.path(), None);
    assert_eq!(
        r.hosts[0].status,
        HostStatus::Changed,
        "{}\n{out}",
        r.render()
    );
    assert_eq!(groups_of(d.path(), "tpdev"), ["tpg"]);
}

//! A persistent shell that dies mid-window, at the executor level: the run
//! returns a report, the host fails at the op that killed the shell, and
//! the op sent behind it is not run. Whether that op's write lands before
//! or after the shell is gone, the send error is the op's failure.

use std::cell::RefCell;
use std::rc::Rc;

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::report::{HostStatus, Outcome};
use telepatch::transport::local::LocalTransport;
use telepatch::{Host, Playbook, PlaybookFuture};

fn kill_then_true(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        // In the op's subshell `$$` is still the persistent shell's pid.
        // The op's own subshell goes too (`$BASHPID`): under `set -m` it is
        // its own process group and would outlive the shell, print its
        // trailer and report rc 0, leaving only the op behind it lost.
        h.raw("kill -9 $$ $BASHPID");
        h.raw("true");
        Ok(())
    })
}

#[test]
fn a_shell_killed_by_its_op_fails_the_host_at_that_op() {
    let out = Rc::new(RefCell::new(String::new()));
    let pb = Playbook::new("death", kill_then_true);
    let report = run(
        &pb,
        vec![HostSpec {
            groups: Vec::new(),
            name: "local".into(),
            target: Target::Local(LocalTransport::new().uid(0)),
            vars: (),
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    let host = &report.hosts[0];
    let HostStatus::Failed { op_id, tail, .. } = &host.status else {
        panic!("{:?}\n{}", host.status, out.borrow())
    };
    assert_eq!(*op_id, 0, "{}", report.render());
    assert!(tail.contains("shell died"), "{tail}");
    assert_eq!(host.ops.len(), 2, "{}", report.render());
    assert_eq!(host.ops[0].outcome, Outcome::Failed);
    assert_eq!(host.ops[1].outcome, Outcome::NotRun);
}

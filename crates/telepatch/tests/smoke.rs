#[test]
fn reexports_macros() {
    // The macros crate is reachable through telepatch's re-export.
    assert_eq!(telepatch::__macros_marker!(), "telepatch-macros");
}

#[test]
fn marker_expands() {
    assert_eq!(telepatch_macros::__macros_marker!(), "telepatch-macros");
}

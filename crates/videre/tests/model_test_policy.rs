//! Model-test policy without loading weights or contacting the Hub.

#[allow(dead_code)] // This target tests policy, not the shared skip printer.
mod model_test_support {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../videre-core/tests/support/model_test_support.rs"
    ));
}

#[test]
fn ci_zero_disables_model_tests_independent_of_cache_state() {
    // This decision must depend on the CI setting, not on cache contents.
    assert!(model_test_support::ci_model_tests_disabled(Some("0")));
}

#[test]
fn absent_or_nonzero_setting_does_not_change_the_local_suite_yet() {
    assert!(!model_test_support::ci_model_tests_disabled(None));
    assert!(!model_test_support::ci_model_tests_disabled(Some("1")));
    assert!(!model_test_support::ci_model_tests_disabled(Some("yes")));
}

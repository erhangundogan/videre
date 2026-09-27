//! Model-test policy without loading weights or contacting the Hub.

#[allow(dead_code)] // This target tests policy, not the shared skip printer.
mod model_test_support {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../videre-core/tests/support/model_test_support.rs"
    ));
}

#[test]
fn final_policy_ignores_cache_warmth_and_requires_explicit_opt_in() {
    for cache_is_warm in [false, true] {
        assert!(
            !model_test_support::parse_model_test_mode(None).unwrap(),
            "warm={cache_is_warm}"
        );
        assert!(
            !model_test_support::parse_model_test_mode(Some("0")).unwrap(),
            "warm={cache_is_warm}"
        );
        assert!(
            model_test_support::parse_model_test_mode(Some("1")).unwrap(),
            "warm={cache_is_warm}"
        );
    }
    assert!(model_test_support::parse_model_test_mode(Some("yes")).is_err());
}

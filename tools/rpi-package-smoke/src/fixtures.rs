//! Fixture lookup for the smoke tests.
//!
//! `task smoke` runs every `--ignored` test, but the tests that exercise one
//! specific artifact need that artifact's path in the environment. A missing
//! fixture means "not applicable on this machine", not "broken": these helpers
//! report the skip loudly and the test returns early, so one unset variable
//! cannot fail the whole smoke run.
//!
//! This module is compiled both into the crate (from `lib.rs`) and into the
//! standalone `tests/run_stats.rs` binary, so it must not depend on anything
//! else in the crate.

use std::path::PathBuf;

/// The path named by `var`, or `None` (with a note) when it is unset or empty.
pub fn fixture_path(var: &str) -> Option<PathBuf> {
    match std::env::var(var) {
        Ok(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => {
            eprintln!("{var} not set; skipping the integration test that needs it");
            None
        }
    }
}

/// Same as [`fixture_path`] for fixtures that are not filesystem paths.
pub fn fixture_var(var: &str) -> Option<String> {
    match std::env::var(var) {
        Ok(value) if !value.is_empty() => Some(value),
        _ => {
            eprintln!("{var} not set; skipping the integration test that needs it");
            None
        }
    }
}

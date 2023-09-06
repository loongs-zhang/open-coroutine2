#![deny(
    // The following are allowed by default lints according to
    // https://doc.rust-lang.org/rustc/lints/listing/allowed-by-default.html
    anonymous_parameters,
    bare_trait_objects,
    // box_pointers, // use box pointer to allocate on heap
    // elided_lifetimes_in_paths, // allow anonymous lifetime
    missing_copy_implementations,
    missing_debug_implementations,
    missing_docs,
    single_use_lifetimes,
    // trivial_casts,
    trivial_numeric_casts,
    // unreachable_pub, allow clippy::redundant_pub_crate lint instead
    // unsafe_code,
    unstable_features,
    unused_extern_crates,
    unused_import_braces,
    unused_qualifications,
    unused_results,
    variant_size_differences,
    warnings, // treat all wanings as errors

    clippy::all,
    // clippy::restriction,
    clippy::pedantic,
    // clippy::nursery, // It's still under development
    clippy::cargo,
)]
#![allow(
    // Some explicitly allowed Clippy lints, must have clear reason to allow
    clippy::blanket_clippy_restriction_lints, // allow clippy::restriction
    clippy::implicit_return, // actually omitting the return keyword is idiomatic Rust code
    clippy::module_name_repetitions, // repeation of module name in a struct name is not big deal
    clippy::multiple_crate_versions, // multi-version dependency crates is not able to fix
    clippy::panic_in_result_fn,
    clippy::shadow_same, // Not too much bad
    clippy::shadow_reuse, // Not too much bad
    clippy::exhaustive_enums,
    clippy::exhaustive_structs,
    clippy::indexing_slicing,
    clippy::wildcard_imports,
    clippy::separated_literal_suffix, // conflicts with clippy::unseparated_literal_suffix
)]

//! see `https://github.com/acl-dev/open-coroutine`
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// get the current wall clock in ns
///
/// # Panics
/// if the time is before `UNIX_EPOCH`
#[must_use]
pub fn now() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("1970-01-01 00:00:00 UTC was {} seconds ago!")
            .as_nanos(),
    )
    .unwrap_or(u64::MAX)
}

/// current ns time add `dur`.
#[must_use]
pub fn get_timeout_time(dur: Duration) -> u64 {
    u64::try_from(dur.as_nanos())
        .map(|d| d.saturating_add(now()))
        .unwrap_or(u64::MAX)
}

/// Coroutine abstraction and impl.
pub mod coroutine;

/// Scheduler abstraction and impl.
pub mod scheduler;

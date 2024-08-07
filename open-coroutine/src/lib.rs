#![deny(
    // The following are allowed by default lints according to
    // https://doc.rust-lang.org/rustc/lints/listing/allowed-by-default.html
    anonymous_parameters,
    bare_trait_objects,
    // elided_lifetimes_in_paths, // allow anonymous lifetime
    missing_copy_implementations,
    missing_debug_implementations,
    // missing_docs,
    single_use_lifetimes,
    // trivial_casts,
    trivial_numeric_casts,
    unreachable_pub,
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

pub use open_coroutine_core::net::config::Config;
pub use open_coroutine_macros::*;

pub mod join;

pub mod coroutine;

extern "C" {
    fn init_config(config: Config);

    fn shutdowns();
}

pub fn init(config: Config) {
    unsafe { init_config(config) };
}

pub fn shutdown() {
    unsafe { shutdowns() };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_link() {
        init(Config::default());
    }
}

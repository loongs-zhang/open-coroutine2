/// Event driven abstraction and impl.
pub mod selector;

#[allow(missing_docs)]
pub mod event_loop;

/// Global config abstraction and impl.
pub mod config;

/// net core impl.
pub mod core;

/// `io_uring` abstraction and impl.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
#[cfg(all(target_os = "linux", feature = "io_uring"))]
pub mod operator;

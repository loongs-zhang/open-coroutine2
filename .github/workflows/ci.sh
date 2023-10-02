#!/usr/bin/env sh

set -ex

CARGO=cargo
if [ "${CROSS}" = "1" ]; then
    export CARGO_NET_RETRY=5
    export CARGO_NET_TIMEOUT=10

    cargo install cross
    CARGO=cross
fi

# If a test crashes, we want to know which one it was.
export RUST_TEST_THREADS=1
export RUST_BACKTRACE=1

# test open-coroutine-core mod
cd "${PROJECT_DIR}"/open-coroutine-core
"${CARGO}" test --target "${TARGET}" --no-default-features --features korosensei
"${CARGO}" test --target "${TARGET}" --no-default-features --features korosensei --release

if [ "${TARGET}" != "riscv64gc-unknown-linux-gnu" ]; then
    "${CARGO}" test --target "${TARGET}" --no-default-features --features preemptive-schedule
fi

"${CARGO}" test --target "${TARGET}" --no-default-features --features preemptive-schedule --release

# test open-coroutine-net mod
cd "${PROJECT_DIR}"/open-coroutine-net
"${CARGO}" test --target "${TARGET}" --no-default-features
"${CARGO}" test --target "${TARGET}" --no-default-features --release

# only test io_uring in x86_64
if [ "${TARGET}" = "x86_64-unknown-linux-gnu" ]; then
    "${CARGO}" test --target "${TARGET}" --no-default-features --features io_uring
    "${CARGO}" test --target "${TARGET}" --no-default-features --features io_uring --release

    "${CARGO}" test --target "${TARGET}" --no-default-features --features compatible --release
fi


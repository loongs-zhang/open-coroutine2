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

# open-coroutine-core korosensei
cd "${PROJECT_DIR}"/open-coroutine-core
"${CARGO}" test --target "${TARGET}" --no-default-features --features korosensei
"${CARGO}" test --target "${TARGET}" --no-default-features --features korosensei --release

# todo io_uring
#if [ "${OS}" = "ubuntu-latest" ]; then
#    "${CARGO}" test $CARGO_TEST_FLAGS --target "${TARGET}" --all-targets --features asm-unwind
#    "${CARGO}" test $CARGO_TEST_FLAGS --target "${TARGET}" --all-targets --features asm-unwind --release
#fi

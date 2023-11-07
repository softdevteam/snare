#! /bin/sh

set -e

export CARGO_HOME="`pwd`/.cargo"
export RUSTUP_HOME="`pwd`/.rustup"

curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs > rustup.sh
sh rustup.sh --default-host x86_64-unknown-linux-gnu --default-toolchain stable -y --no-modify-path

export PATH=`pwd`/.cargo/bin/:$PATH

cargo fmt --all -- --check
cargo test

mkdir test_install
PREFIX=test_install make install
test -f test_install/bin/snare

which cargo-deny | cargo install cargo-deny || true
if cargo deny --version 2>&1 > /dev/null; then
    cargo-deny check license
else
    echo "Warning: couldn't run cargo-deny" > /dev/stderr
fi

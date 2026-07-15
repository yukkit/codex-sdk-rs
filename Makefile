CARGO_INSTALL_ARGS ?= --locked
CARGO_DENY_INSTALL_ARGS ?= $(CARGO_INSTALL_ARGS)
CARGO_UDEPS_INSTALL_ARGS ?= $(CARGO_INSTALL_ARGS)

setup: setup-cargo-deny setup-nightly setup-cargo-udeps setup-clippy

setup-cargo-deny:
	@if cargo deny --version >/dev/null 2>&1; then \
		echo "cargo-deny already installed: $$(cargo deny --version)"; \
	else \
		echo "Installing cargo-deny..."; \
		cargo install cargo-deny $(CARGO_DENY_INSTALL_ARGS); \
	fi

setup-nightly:
	@if rustup run nightly cargo fmt --version >/dev/null 2>&1; then \
		echo "nightly rustfmt already installed: $$(rustup run nightly cargo fmt --version)"; \
	elif command -v rustup >/dev/null 2>&1; then \
		echo "Installing nightly toolchain with rustfmt..."; \
		rustup toolchain install nightly --component rustfmt; \
	else \
		echo "rustup is required to install the nightly toolchain" >&2; \
		exit 1; \
	fi

setup-cargo-udeps:
	@if cargo udeps --version >/dev/null 2>&1; then \
		echo "cargo-udeps already installed: $$(cargo udeps --version)"; \
	else \
		echo "Installing cargo-udeps..."; \
		cargo install cargo-udeps $(CARGO_UDEPS_INSTALL_ARGS); \
	fi

setup-clippy:
	@if cargo clippy --version >/dev/null 2>&1; then \
		echo "clippy already installed: $$(cargo clippy --version)"; \
	elif command -v rustup >/dev/null 2>&1; then \
		echo "Installing clippy..."; \
		rustup component add clippy; \
	else \
		echo "rustup is required to install clippy" >&2; \
		exit 1; \
	fi

deny_check: setup-cargo-deny
	cargo deny check

udeps_check: setup-nightly setup-cargo-udeps
	cargo +nightly udeps --all-targets --all-features --quiet

docs_check:
	cargo doc --no-deps --document-private-items --all-features

docs:
	cargo doc --no-deps --document-private-items --all-features --open

fmt_check: setup-nightly
	cargo +nightly fmt --all -- --check

fmt: setup-nightly
	cargo +nightly fmt --all

clippy_check: setup-clippy
	cargo clippy --workspace --all-targets -- -D warnings

clippy: setup-clippy
	cargo clippy --workspace --all-targets --fix --allow-staged

build:
	cargo build --workspace --bins

build_release:
	cargo build --release --workspace --bins

doc-test:
	cargo test --no-fail-fast --doc --all-features --workspace

unit-test: doc-test
	cargo test --no-fail-fast --lib --all-features --workspace

test: doc-test
	cargo test --no-fail-fast --all-targets --all-features --workspace

check: deny_check fmt_check clippy_check build test docs_check

clean:
	cargo clean

run:
	cargo run -- run

.PHONY: setup setup-cargo-deny setup-nightly setup-cargo-udeps setup-clippy docs check fmt fmt_check clippy clippy_check build build_release test docs_check clean run

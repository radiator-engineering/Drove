.PHONY: ci fmt clippy test docs audit

# Run the same checks CI runs, in the same order.
ci: fmt clippy test docs audit

fmt:
	cargo fmt --all --check

clippy:
	cargo clippy --all-targets --all-features --locked -- -D warnings

test:
	cargo test --all-targets --all-features --locked

docs:
	RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features --locked

audit:
	cargo audit

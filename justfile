# Point git at the committed hooks (idempotent; safe to re-run).
setup-dev:
	git config core.hooksPath .githooks

pre-commit:
	cargo fmt --all --check
	cargo doc --no-deps --all-features
	cargo test --all-features


# Point git at the committed hooks (idempotent; safe to re-run).
setup-git-hooks:
	git config core.hooksPath .githooks

# Checks run by the pre-commit hook. `-D warnings` on rustdoc mirrors CI so a
# broken intra-doc link fails here instead of in the docs job.
pre-commit:
	cargo fmt --all --check
	RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features

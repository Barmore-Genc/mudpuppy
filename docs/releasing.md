# Releasing

Distribution is handled by [cargo-dist](https://opensource.axo.dev/cargo-dist/).
Pushing a version tag builds prebuilt binaries and installers and publishes them
to a GitHub Release. No local toolchain or manual upload is involved.

## Cut a release

```sh
# 1. Bump `version` in Cargo.toml, commit, and merge to main.
# 2. Tag the merge commit and push the tag:
git tag v0.2.0
git push origin v0.2.0
```

The tag push triggers `.github/workflows/release.yml`, which builds every target,
generates the installers, and publishes the GitHub Release. Tags with a
prerelease suffix (e.g. `v0.2.0-rc.1`) are marked as GitHub pre-releases.

## Targets

| Platform | Triple |
| --- | --- |
| macOS arm64 | `aarch64-apple-darwin` |
| macOS x86_64 | `x86_64-apple-darwin` |
| Linux x86_64 | `x86_64-unknown-linux-gnu` |
| Linux arm64 | `aarch64-unknown-linux-gnu` |
| Windows x86_64 | `x86_64-pc-windows-msvc` |
| Windows arm64 | `aarch64-pc-windows-msvc` |

Each ships as a `.tar.xz`/`.zip` archive with a `.sha256`, alongside a shell and
a PowerShell installer.

## Config

The target matrix and installers live in `dist-workspace.toml`. The release
workflow is generated from it — after editing the config, regenerate and commit:

```sh
dist generate
```

CI runs `dist generate --check` on pull requests, so a hand-edited or stale
`release.yml` fails the build.

> **musl note:** `x86_64-unknown-linux-musl` is intentionally omitted. The vendored
> Luau interpreter is C++, and a stock runner's `musl-tools` ships no
> musl-targeting C++ compiler. Re-adding it needs a full musl C++ toolchain (e.g.
> the `rust-musl-cross` container).

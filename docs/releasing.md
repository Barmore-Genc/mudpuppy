# Releasing

Distribution is handled by [cargo-dist](https://opensource.axo.dev/cargo-dist/).
Pushing a version tag builds prebuilt binaries and installers and publishes them
to a GitHub Release. No local toolchain or manual upload is involved.

## Cut a release

```sh
# 1. Bump `version` in Cargo.toml.
# 2. In CHANGELOG.md, rename the `## Unreleased` section to the new version
#    (e.g. `## 0.2.0`) and add a fresh empty `## Unreleased` above it.
# 3. Commit and merge to main.
# 4. Tag the merge commit and push the tag:
git tag v0.2.0
git push origin v0.2.0
```

The tag push triggers `.github/workflows/release.yml`, which builds every target,
generates the installers, and publishes the GitHub Release. Tags with a
prerelease suffix (e.g. `v0.2.0-rc.1`) are marked as GitHub pre-releases.

## Changelog

Every release **must** have a matching section in [CHANGELOG.md](../CHANGELOG.md).
The heading has to match the tag's version (`## 0.2.0` for tag `v0.2.0`) — `dist`
extracts that section into the published `dist-manifest.json` as the release notes,
and mudpuppy's in-app update prompt reads them from there to show users what
changed. A missing or mis-titled section means an empty changelog in the prompt.

Write the entry as you go: keep a running `## Unreleased` section at the top of
`CHANGELOG.md` and add a bullet under it (grouped `Added`/`Changed`/`Fixed`) with
every user-visible change, then rename it to the version when cutting the release.
This applies whether a human or an AI agent prepares the release.

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

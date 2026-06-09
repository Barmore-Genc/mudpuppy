---
name: create-release
description: Cut a new mudpuppy release — bump the version, update the changelog, and tag. Use when asked to create/cut a release, ship a version, or tag vX.Y.Z.
---

# Creating a mudpuppy release

Releases are driven by cargo-dist: pushing a `vX.Y.Z` tag triggers
`.github/workflows/release.yml`, which builds the binaries and creates the
GitHub Release. The release notes come from the `CHANGELOG.md` section whose
heading matches the tagged version, so the changelog and the version bump must
already be on the tagged commit.

`main` is protected, so the version bump and changelog go through a PR first;
the tag is pushed only **after** that PR merges.

## Steps

1. **Find the previous release tag and the commits since.** The latest tag is
   the prior release:

   ```sh
   git fetch --tags
   git describe --tags --abbrev=0   # e.g. v0.1.1
   git log --oneline v0.1.1..origin/main
   ```

   Use that commit list to write the changelog entry. Only include what landed
   since the last tag.

2. **Prepend a new section to `CHANGELOG.md`.** Add it at the top of the entry
   list, newest-first, with a heading that matches the version you're about to
   release (Keep a Changelog format):

   ```markdown
   ## 0.2.0 - YYYY-MM-DD

   ### Added
   - ...
   ```

   The heading version must exactly match the tag (`v0.2.0` -> `## 0.2.0`), or
   cargo-dist falls back to GitHub's auto-generated notes. The CI `changelog`
   job enforces that a matching section exists.

3. **Bump the version in `Cargo.toml`**, then sync the lockfile:

   ```sh
   # edit `version = "..."` under [package] in Cargo.toml
   cargo update -p mudpuppy --precise X.Y.Z
   ```

4. **Open a PR** with the bump + changelog and let it merge (CI must be green —
   the `changelog` job verifies the section matches the new version).

5. **Once the PR lands, push the tag** to start the real release:

   ```sh
   git checkout main && git pull
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

   This kicks off `release.yml`. Do not push the tag before the bump is on
   `main`: cargo-dist refuses to release if the tag doesn't match the
   `Cargo.toml` version at that commit.

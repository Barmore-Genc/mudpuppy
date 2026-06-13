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
   git describe --tags --abbrev=0   # e.g. v0.2.0
   git log --oneline v0.2.0..origin/main
   ```

   Read the actual changes, not just the commit subjects — open the PRs or
   diffs as needed to understand what each change means *for a user*. The
   changelog is written for users, not from commit messages (see "Writing the
   changelog" below).

2. **Choose the version bump.** mudpuppy is an application, not a library —
   nothing is a "breaking change" to a downstream API, so classic SemVer doesn't
   really apply. It's a judgment call on how big the release feels:

   - **Patch bump** (`0.3.0` → `0.3.1`) — only small fixes or polish, nothing a
     user would call a new feature.
   - **Minor bump** (`0.2.0` → `0.3.0`) — the default for a release that lands
     one or more real user-facing features. This is the usual case while pre-1.0.
   - **Major bump** (`0.x` → `1.0.0`) — reserved: the jump to `1.0.0` is the
     "we call this production ready" milestone, not an automatic consequence of
     any change. Don't bump to `1.x` unless the user explicitly decides
     mudpuppy is production ready. **After** `1.0`, a major bump is again just a
     vibes call — "is this significant enough to be a whole new version?" — not
     a SemVer breaking-change signal.

   When unsure which level, ask the user — "is this enough of a feature bump to
   call it a new version?" is a vibes call that's theirs to make.

3. **Prepend a new section to `CHANGELOG.md`** at the top of the entry list,
   newest-first, with a heading that matches the version you're releasing (Keep
   a Changelog format):

   ```markdown
   ## 0.3.0 - YYYY-MM-DD

   ### Added
   - ...

   ### Changed
   - ...

   ### Fixed
   - ...
   ```

   The heading version must exactly match the tag (`v0.3.0` -> `## 0.3.0`), or
   cargo-dist falls back to GitHub's auto-generated notes. The CI `changelog`
   job enforces that a matching section exists. Omit any of the three
   subsections that have no entries.

4. **Bump the version in `Cargo.toml`**, then sync the lockfile:

   ```sh
   # edit `version = "..."` under [package] in Cargo.toml
   cargo update -p mudpuppy --precise X.Y.Z
   ```

5. **Open a PR** with the bump + changelog (branch off `main` — it's protected)
   and let the user read it. CI must be green; the `changelog` job verifies the
   section matches the new version.

6. **Once the PR lands, push the tag** to start the real release:

   ```sh
   git checkout main && git pull --ff-only
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

   This kicks off `release.yml`. Do not push the tag before the bump is on
   `main`: cargo-dist refuses to release if the tag doesn't match the
   `Cargo.toml` version at that commit. Watch the run
   (`gh run watch <id> --exit-status`) and confirm the GitHub Release was
   published with its assets.

## Writing the changelog

The changelog is the single most important part of the release, and the easiest
to get wrong. It is **release notes for users**, not a development log. Write it
from the reader's side of the screen.

### What to include

Only changes a user can **see, interact with, or configure**:

- New features, commands, keybindings, and options.
- Changes to existing behavior, defaults, or keybindings they'll notice.
- Bug fixes for problems a user could actually hit.

### What to leave out

If a change is invisible to someone just *using* mudpuppy, it does not belong in
the changelog:

- Refactors, renames, module splits, type changes, internal API churn.
- Test additions, CI/workflow changes, lint/format/dependency bumps (unless a
  dep bump fixes a user-visible bug — then describe the *bug fix*, not the bump).
- Performance work with no perceptible effect; scaffolding and groundwork.

When in doubt, ask: "Would a user notice this if I didn't tell them?" If no,
leave it out. A short, honest changelog beats a padded one.

### How to write each entry

- **Describe what shipped, from the user's point of view.** State what they can
  now do or what now behaves differently — not how it was built.
- **Never narrate the development process or alternatives.** Users don't know or
  care what you planned, considered, or rejected. No "we switched from X to Y",
  "instead of a modal we now…", "this replaces the old approach". Just state the
  shipped behavior. (If contrast genuinely helps a user understand a *changed*
  behavior, describe the old and new behavior plainly — not the decision.)
- **Lead with the user benefit**, then the mechanism if it helps.
- **Group** under `Added` / `Changed` / `Fixed`. A removal or a changed default
  goes under `Changed`.
- **Reference the PR** at the end of each entry: `(#53)`.
- Keep it concrete and skimmable; wrap prose, one bullet per change.

### Examples

These are drawn from the real `0.3.0` section — match this voice:

Good (user-facing, describes the shipped behavior and its benefit):

```markdown
- Horizontal scrolling in the diff pane: pan long lines with `h`/`l` or the
  arrow keys (#53).
- The agent can supply a comment body from a file or stdin with `--body-file`,
  avoiding the shell-quoting approval prompts that multi-line `--body` triggers
  (#49).
```

Bad (narrates the decision / internals; rewrite to state what users get):

```markdown
- Replaced the centered modal composer with an inline one because the modal
  covered the diff.            ← decision narration; users don't care why
- Refactored anchor.rs to extract a relocation cascade.   ← invisible internals
- We considered a popup but went with inline threads instead.  ← alternatives
```

The good version of the first bad line is the real `0.3.0` entry: "Comments
render as inline threads spliced under the line they annotate, and the composer
opens inline under the cursor… instead of as a centered modal (#50)" — it
describes the *behavior*, and the "instead of a modal" contrast is about what
the user sees, not about the decision.

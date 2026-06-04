//! Self-update support: ask GitHub whether a newer release exists, and install a
//! requested version.
//!
//! Like [`crate::source`], this makes no network calls of its own — it shells out
//! to `gh` (read-only `gh api`) to read the latest release tag, and to `cargo` to
//! install a new version. Both are the project's existing "lean, local toolchain"
//! pattern (no bundled HTTP client). Everything here is pure version arithmetic
//! plus those two subprocess calls; the UI policy (when to check, how to prompt)
//! lives in `core.luau`, driven through the `mudpuppy.updates` Lua table.
//!
//! Security: [`install`] only ever runs after [`is_valid_version_tag`] accepts the
//! argument — a strict `vMAJOR.MINOR.PATCH` shape, digits only. Combined with
//! arg-separated spawning (never a shell string) this keeps a version value, even
//! one that reached us from a script, from smuggling extra arguments or shell
//! metacharacters into the subprocess.

use std::process::Command;

use anyhow::{bail, Context, Result};

/// This build's version, e.g. `"0.1.1"` (no leading `v`), from Cargo.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The `owner/repo` slug parsed from the crate's `repository` URL, used to address
/// the GitHub releases API. `None` if the repository isn't a recognizable GitHub
/// URL (then there's nothing to check against).
pub fn repo_slug() -> Option<String> {
    let url = env!("CARGO_PKG_REPOSITORY");
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    (!rest.is_empty()).then(|| rest.to_string())
}

/// Check GitHub for the latest release. Returns `Some(tag)` (e.g. `"v1.2.3"`) when
/// it is newer than the running build, `None` when up to date, the latest can't be
/// determined (no releases yet), or the tag isn't comparable.
///
/// Errors only when the `gh` subprocess itself can't run; a missing binary or a
/// 404 (no releases) is reported as "no newer version" so a user without `gh`
/// simply never sees update prompts.
pub fn check() -> Result<Option<String>> {
    let Some(slug) = repo_slug() else {
        return Ok(None);
    };
    let endpoint = format!("repos/{slug}/releases/latest");
    // `--jq` extracts the tag; a 404 (no published releases) surfaces as a soft
    // miss (`Ok(None)`) rather than an error.
    let out = match crate::source::run_optional("gh", &["api", &endpoint, "--jq", ".tag_name"]) {
        Ok(Some(out)) => out,
        // No releases, or `gh` not installed / not authenticated: nothing to
        // offer, not a hard failure.
        Ok(None) => return Ok(None),
        Err(_) => return Ok(None),
    };
    let tag = out.trim();
    if tag.is_empty() {
        return Ok(None);
    }
    Ok(is_newer(tag, current_version()).then(|| tag.to_string()))
}

/// Install `version` (a `vMAJOR.MINOR.PATCH` tag), replacing the current binary.
///
/// The version is validated by [`is_valid_version_tag`] before it reaches the
/// subprocess; an invalid value is refused outright. Shells out to
/// `cargo install mudpuppy --locked --version <semver>` (crates.io is the
/// canonical published source). The running process keeps executing the old code
/// until it is restarted.
pub fn install(version: &str) -> Result<()> {
    if !is_valid_version_tag(version) {
        bail!("refusing to update to {version:?}: not a vMAJOR.MINOR.PATCH version tag");
    }
    // Safe: `is_valid_version_tag` guaranteed the leading `v`.
    let semver = version.strip_prefix('v').unwrap();
    let status = Command::new("cargo")
        .args([
            "install",
            env!("CARGO_PKG_NAME"),
            "--locked",
            "--version",
            semver,
        ])
        .status()
        .context("running `cargo install` to update mudpuppy")?;
    if !status.success() {
        bail!("`cargo install` exited unsuccessfully ({status})");
    }
    Ok(())
}

/// Whether `tag` is a release tag we will pass to [`install`]: exactly
/// `v<digits>.<digits>.<digits>`. Rejects anything with extra components, missing
/// parts, non-digits, or stray characters (slashes, shell metacharacters, version
/// ranges) — the validation boundary keeping a version value from becoming an
/// argument- or shell-injection vector.
pub fn is_valid_version_tag(tag: &str) -> bool {
    let Some(rest) = tag.strip_prefix('v') else {
        return false;
    };
    let parts: Vec<&str> = rest.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether release `latest` is strictly newer than `current` by semantic version.
/// Either may carry a leading `v`; pre-release/build metadata after the patch
/// number is ignored. Unparseable input compares as "not newer" so a malformed
/// tag never prompts an update.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_semver(latest), parse_semver(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// Parse `[v]MAJOR.MINOR.PATCH[-pre][+build]` into a comparable tuple, ignoring any
/// pre-release/build suffix. `None` if the core isn't three dotted integers.
fn parse_semver(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim();
    let s = s.strip_prefix('v').unwrap_or(s);
    // Drop anything from the first `-`/`+`: we only order on the release core.
    let core = s.split(['-', '+']).next()?;
    let mut it = core.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    let patch = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_ordering() {
        assert!(is_newer("v1.2.4", "1.2.3"));
        assert!(is_newer("1.3.0", "v1.2.9"));
        assert!(is_newer("2.0.0", "1.99.99"));
        assert!(!is_newer("v1.2.3", "1.2.3"));
        assert!(!is_newer("v1.2.2", "1.2.3"));
        // Pre-release/build metadata is ignored down to the release core.
        assert!(!is_newer("v1.2.3-rc1", "1.2.3"));
        assert!(is_newer("v1.2.4-rc1", "1.2.3"));
    }

    #[test]
    fn unparseable_versions_are_not_newer() {
        assert!(!is_newer("latest", "1.2.3"));
        assert!(!is_newer("v1.2", "1.2.3"));
        assert!(!is_newer("v1.2.3.4", "1.2.3"));
        assert!(!is_newer("", "1.2.3"));
    }

    #[test]
    fn valid_version_tag_accepts_only_strict_triples() {
        assert!(is_valid_version_tag("v1.2.3"));
        assert!(is_valid_version_tag("v0.0.0"));
        assert!(is_valid_version_tag("v10.20.30"));
    }

    #[test]
    fn valid_version_tag_rejects_injection_and_malformed_input() {
        // Missing prefix, wrong arity, non-digits.
        assert!(!is_valid_version_tag("1.2.3"));
        assert!(!is_valid_version_tag("v1.2"));
        assert!(!is_valid_version_tag("v1.2.3.4"));
        assert!(!is_valid_version_tag("v1.2.x"));
        assert!(!is_valid_version_tag("vfoo"));
        // Anything that could smuggle extra args or shell metacharacters.
        assert!(!is_valid_version_tag("v1.2.3 && rm -rf /"));
        assert!(!is_valid_version_tag("v1.2.3;reboot"));
        assert!(!is_valid_version_tag("../../etc/passwd"));
        assert!(!is_valid_version_tag("v1.2.3/extra"));
        assert!(!is_valid_version_tag("v-1.2.3"));
        assert!(!is_valid_version_tag(""));
    }

    #[test]
    fn install_refuses_invalid_versions_without_spawning() {
        // The guard fires before any subprocess; the error names the offender.
        let err = install("v1.2.3; rm -rf /").unwrap_err();
        assert!(err.to_string().contains("refusing to update"));
    }

    #[test]
    fn repo_slug_parses_the_cargo_repository() {
        // The crate's own repository is a GitHub URL, so this resolves.
        let slug = repo_slug().expect("repository slug");
        assert!(slug.contains('/'), "slug should be owner/repo: {slug}");
        assert!(!slug.starts_with("http"));
    }
}

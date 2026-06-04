//! Self-update support: ask whether a newer release exists, and install it.
//!
//! The version check reads the release `dist-manifest.json` that the project
//! publishes to GitHub Pages (`pages.yml`), over a single plain HTTPS GET — so it
//! needs no `gh` (the GitHub CLI is *not* a hard dependency of mudpuppy). The
//! fetch is a blocking [`ureq`] call; callers in the TUI run it on a
//! `tokio::task::spawn_blocking` thread so the event loop never stalls.
//! [`check`] is split from [`check_with`] precisely so tests can drive the parse +
//! comparison with a mocked fetcher instead of touching the network.
//!
//! Installing still shells out to `cargo`. Security: [`install`] only ever runs
//! after [`is_valid_version_tag`] accepts the argument — a strict
//! `vMAJOR.MINOR.PATCH` shape, digits only. Combined with arg-separated spawning
//! (never a shell string) this keeps a version value, even one that reached us
//! from a script or the network, from smuggling extra arguments or shell
//! metacharacters into the subprocess.

use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// Environment variable overriding the manifest URL (used by tests and anyone
/// hosting the manifest elsewhere). When unset the URL is derived from the crate's
/// repository as the GitHub Pages `dist-manifest.json`.
pub const MANIFEST_URL_ENV: &str = "MUDPUPPY_UPDATE_MANIFEST_URL";

/// How long the launch-time manifest GET may take before giving up. Bounded so a
/// hung connection can't wedge a caller (and the spawn_blocking thread) forever.
const FETCH_TIMEOUT: Duration = Duration::from_secs(8);

/// This build's version, e.g. `"0.1.1"` (no leading `v`), from Cargo.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The `owner/repo` slug parsed from the crate's `repository` URL. `None` if the
/// repository isn't a recognizable GitHub URL (then there's nowhere to check).
pub fn repo_slug() -> Option<String> {
    let url = env!("CARGO_PKG_REPOSITORY");
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    (!rest.is_empty()).then(|| rest.to_string())
}

/// The URL of the published release manifest: `$MUDPUPPY_UPDATE_MANIFEST_URL` if
/// set, else the GitHub Pages `dist-manifest.json` for this repo
/// (`https://<owner>.github.io/<repo>/dist-manifest.json`). `None` only when no
/// repository slug can be derived and no override is set.
pub fn manifest_url() -> Option<String> {
    if let Some(url) = std::env::var_os(MANIFEST_URL_ENV).filter(|v| !v.is_empty()) {
        return Some(url.to_string_lossy().into_owned());
    }
    let slug = repo_slug()?;
    let (owner, repo) = slug.split_once('/')?;
    Some(format!(
        "https://{}.github.io/{repo}/dist-manifest.json",
        owner.to_lowercase()
    ))
}

/// Check for a newer release. Returns `Some(tag)` (e.g. `"v1.2.3"`) when the
/// published manifest names a version newer than this build, else `None`. Errors
/// only when the network fetch or the manifest parse fails — the TUI's Lua wrapper
/// turns those into "no update" so a transient failure never disturbs a session.
pub fn check() -> Result<Option<String>> {
    check_with(http_get)
}

/// The testable core of [`check`]: resolve the manifest URL, hand it to `fetch`,
/// parse the result, and compare against the running version. `fetch` is the seam
/// tests mock — they pass a closure returning canned manifest JSON (or an error)
/// instead of hitting the network.
fn check_with(fetch: impl FnOnce(&str) -> Result<String>) -> Result<Option<String>> {
    let Some(url) = manifest_url() else {
        return Ok(None);
    };
    let body = fetch(&url)?;
    let latest = version_from_manifest(&body)?;
    Ok(latest.filter(|tag| is_newer(tag, current_version())))
}

/// The slice of a dist `dist-manifest.json` we read: the announcement tag and the
/// per-app releases. Everything else (artifacts, hashes, …) is ignored.
#[derive(Deserialize)]
struct DistManifest {
    #[serde(default)]
    announcement_tag: Option<String>,
    #[serde(default)]
    releases: Vec<ManifestRelease>,
}

#[derive(Deserialize)]
struct ManifestRelease {
    app_name: String,
    app_version: String,
}

/// Pull the released version (as a `vX.Y.Z` tag) out of a `dist-manifest.json`.
/// Prefers the manifest's `announcement_tag`; falls back to the `app_version` of
/// the release matching this crate (or the first release). `None` if the manifest
/// names no version.
fn version_from_manifest(json: &str) -> Result<Option<String>> {
    let manifest: DistManifest =
        serde_json::from_str(json).context("parsing dist-manifest.json")?;

    if let Some(tag) = manifest.announcement_tag.filter(|t| !t.trim().is_empty()) {
        return Ok(Some(normalize_tag(tag.trim())));
    }
    let version = manifest
        .releases
        .iter()
        .find(|r| r.app_name == env!("CARGO_PKG_NAME"))
        .or_else(|| manifest.releases.first())
        .map(|r| normalize_tag(r.app_version.trim()));
    Ok(version)
}

/// Ensure a version string carries the leading `v` the rest of the module (and
/// the install validation) expects.
fn normalize_tag(s: &str) -> String {
    if s.starts_with('v') {
        s.to_string()
    } else {
        format!("v{s}")
    }
}

/// Fetch `url` over HTTPS, returning the body as a string. A non-2xx status, a
/// connection failure, or a timeout is an error. Bounded by [`FETCH_TIMEOUT`].
fn http_get(url: &str) -> Result<String> {
    let agent = ureq::AgentBuilder::new().timeout(FETCH_TIMEOUT).build();
    let body = agent
        .get(url)
        .call()
        .with_context(|| format!("fetching {url}"))?
        .into_string()
        .context("reading the manifest response body")?;
    Ok(body)
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

    /// A minimal dist-manifest.json with the given announcement tag.
    fn manifest_with_tag(tag: &str) -> String {
        format!(r#"{{ "announcement_tag": "{tag}", "releases": [] }}"#)
    }

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
        let slug = repo_slug().expect("repository slug");
        assert!(slug.contains('/'), "slug should be owner/repo: {slug}");
        assert!(!slug.starts_with("http"));
    }

    #[test]
    fn manifest_url_defaults_to_github_pages() {
        // No override → the lowercased-owner github.io URL for this repo.
        std::env::remove_var(MANIFEST_URL_ENV);
        let url = manifest_url().expect("a manifest url");
        assert!(url.starts_with("https://"), "{url}");
        assert!(
            url.ends_with(".github.io/mudpuppy/dist-manifest.json"),
            "{url}"
        );
    }

    #[test]
    fn version_from_manifest_prefers_the_announcement_tag() {
        let json = manifest_with_tag("v2.3.4");
        assert_eq!(
            version_from_manifest(&json).unwrap().as_deref(),
            Some("v2.3.4")
        );
    }

    #[test]
    fn version_from_manifest_falls_back_to_app_version_and_adds_v() {
        let json = r#"{ "releases": [ { "app_name": "mudpuppy", "app_version": "3.1.0" } ] }"#;
        assert_eq!(
            version_from_manifest(json).unwrap().as_deref(),
            Some("v3.1.0"),
            "bare app_version is normalized with a leading v"
        );
    }

    #[test]
    fn version_from_manifest_is_none_when_no_version_is_named() {
        let json = r#"{ "releases": [] }"#;
        assert_eq!(version_from_manifest(json).unwrap(), None);
    }

    #[test]
    fn check_with_offers_only_a_strictly_newer_version() {
        // A far-future manifest → an update is offered.
        let newer = check_with(|_url| Ok(manifest_with_tag("v9.9.9"))).unwrap();
        assert_eq!(newer.as_deref(), Some("v9.9.9"));

        // The running version itself → nothing to offer.
        let same = check_with(|_url| Ok(manifest_with_tag(current_version()))).unwrap();
        assert_eq!(same, None);

        // An older version → nothing to offer.
        let older = check_with(|_url| Ok(manifest_with_tag("v0.0.0"))).unwrap();
        assert_eq!(older, None);
    }

    #[test]
    fn check_with_propagates_a_fetch_error() {
        let err = check_with(|_url| bail!("network down")).unwrap_err();
        assert!(err.to_string().contains("network down"));
    }

    #[test]
    fn check_with_errors_on_unparseable_manifest() {
        let err = check_with(|_url| Ok("not json".to_string())).unwrap_err();
        assert!(err.to_string().contains("dist-manifest.json"));
    }
}

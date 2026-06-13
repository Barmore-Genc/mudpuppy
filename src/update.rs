//! Self-update support: ask whether a newer release exists, and install it.
//!
//! The version check reads the release `dist-manifest.json` that the project
//! publishes to GitHub Pages (`pages.yml`), over a single plain HTTPS GET — so it
//! needs no `gh` (the GitHub CLI is *not* a hard dependency of mudpuppy). The
//! fetch is a blocking [`ureq`] call; callers in the TUI run it on a
//! `tokio::task::spawn_blocking` thread so the event loop never stalls.
//! [`check`] is split from a private `check_with` helper precisely so tests can
//! drive the parse + comparison with a mocked fetcher instead of touching the
//! network.
//!
//! Installing downloads the **prebuilt** release binary and swaps it in — it does
//! *not* shell out to `cargo`, so a user with no Rust toolchain can still update.
//! [`install`] re-reads the same manifest to find the archive built for this
//! binary's target triple (captured at build time, see `build.rs`), downloads it
//! over HTTPS from the GitHub release, verifies it against the SHA-256 the
//! manifest carries, extracts the executable, and replaces the running binary in
//! place (`self_replace`, which handles the running-process specifics on both
//! Unix and Windows). The macOS/Linux archives are `.tar.xz`, Windows ships
//! `.zip`; the codec is chosen per-platform.
//!
//! Security: [`install`] only proceeds after [`is_valid_version_tag`] accepts the
//! requested version — a strict `vMAJOR.MINOR.PATCH` shape, digits only — and the
//! download is rejected unless its checksum matches the manifest. The version
//! never reaches a shell, so it can't smuggle arguments or metacharacters.

use std::collections::HashMap;
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

/// How long an artifact download may take. Larger than [`FETCH_TIMEOUT`] (a
/// release archive is a few MB, not a few KB), but still bounded so an update
/// can't hang indefinitely.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);

/// The target triple this binary was built for (e.g. `aarch64-apple-darwin`),
/// captured from cargo's `TARGET` by `build.rs`. Used to pick the matching
/// prebuilt artifact out of the release manifest.
fn target_triple() -> &'static str {
    env!("MUDPUPPY_TARGET_TRIPLE")
}

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

/// A newer release the user can install: its tag and, when the manifest carries
/// one, the release changelog (the dist `announcement_changelog`, markdown text)
/// so the update prompt can show what changed.
#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    pub version: String,
    pub changelog: Option<String>,
}

/// Check for a newer release. Returns the release (tag + changelog) when the
/// published manifest names a version newer than this build, else `None`. Errors
/// only when the network fetch or the manifest parse fails — the TUI's Lua wrapper
/// turns those into "no update" so a transient failure never disturbs a session.
pub fn check() -> Result<Option<ReleaseInfo>> {
    check_with(http_get)
}

/// The testable core of [`check`]: resolve the manifest URL, hand it to `fetch`,
/// parse the result, and compare against the running version. `fetch` is the seam
/// tests mock — they pass a closure returning canned manifest JSON (or an error)
/// instead of hitting the network.
fn check_with(fetch: impl FnOnce(&str) -> Result<String>) -> Result<Option<ReleaseInfo>> {
    let Some(url) = manifest_url() else {
        return Ok(None);
    };
    let body = fetch(&url)?;
    let Some(release) = release_from_manifest(&body)? else {
        return Ok(None);
    };
    Ok(is_newer(&release.version, current_version()).then_some(release))
}

/// The slice of a dist `dist-manifest.json` we read: the announcement tag, the
/// per-app releases, and the artifact table the install step resolves against.
#[derive(Deserialize)]
struct DistManifest {
    #[serde(default)]
    announcement_tag: Option<String>,
    /// The release changelog (markdown), shown in the update prompt.
    #[serde(default)]
    announcement_changelog: Option<String>,
    #[serde(default)]
    releases: Vec<ManifestRelease>,
    /// Keyed by artifact file name (e.g. `mudpuppy-aarch64-apple-darwin.tar.xz`).
    #[serde(default)]
    artifacts: HashMap<String, ManifestArtifact>,
}

#[derive(Deserialize)]
struct ManifestRelease {
    app_name: String,
    app_version: String,
    /// Names of the artifacts belonging to this release (keys into `artifacts`).
    #[serde(default)]
    artifacts: Vec<String>,
    #[serde(default)]
    hosting: Hosting,
}

#[derive(Deserialize, Default)]
struct Hosting {
    #[serde(default)]
    github: Option<GithubHosting>,
}

/// Where a GitHub-hosted release's files live. The download URL is
/// `artifact_base_url` + `artifact_download_path` + `/` + the artifact name.
#[derive(Deserialize)]
struct GithubHosting {
    artifact_base_url: String,
    artifact_download_path: String,
}

#[derive(Deserialize)]
struct ManifestArtifact {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    target_triples: Vec<String>,
    #[serde(default)]
    assets: Vec<ArtifactAsset>,
    /// Checksums keyed by algorithm; we use `sha256`.
    #[serde(default)]
    checksums: HashMap<String, String>,
}

#[derive(Deserialize)]
struct ArtifactAsset {
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: String,
}

/// Parse `json` into the release (tag + changelog) it names, or `None` if it names
/// no version. The changelog is trimmed and dropped if empty.
fn release_from_manifest(json: &str) -> Result<Option<ReleaseInfo>> {
    let manifest: DistManifest =
        serde_json::from_str(json).context("parsing dist-manifest.json")?;
    let Some(version) = manifest_version(&manifest) else {
        return Ok(None);
    };
    let changelog = manifest
        .announcement_changelog
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_string);
    Ok(Some(ReleaseInfo { version, changelog }))
}

/// The released version (as a `vX.Y.Z` tag) named by a parsed manifest. Prefers
/// the `announcement_tag`; falls back to the `app_version` of the release matching
/// this crate (or the first release). `None` if the manifest names no version.
fn manifest_version(manifest: &DistManifest) -> Option<String> {
    if let Some(tag) = manifest
        .announcement_tag
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        return Some(normalize_tag(tag));
    }
    manifest
        .releases
        .iter()
        .find(|r| r.app_name == env!("CARGO_PKG_NAME"))
        .or_else(|| manifest.releases.first())
        .map(|r| normalize_tag(r.app_version.trim()))
}

/// Pull the released version (as a `vX.Y.Z` tag) out of a `dist-manifest.json`.
/// `None` if the manifest names no version. A thin JSON wrapper over
/// `manifest_version`, kept for the focused tag-extraction tests.
#[cfg(test)]
fn version_from_manifest(json: &str) -> Result<Option<String>> {
    let manifest: DistManifest =
        serde_json::from_str(json).context("parsing dist-manifest.json")?;
    Ok(manifest_version(&manifest))
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

/// Fetch `url` over HTTPS, returning the raw bytes. Used for the (binary) release
/// archive. Bounded by [`DOWNLOAD_TIMEOUT`].
fn http_get_bytes(url: &str) -> Result<Vec<u8>> {
    use std::io::Read;
    let agent = ureq::AgentBuilder::new().timeout(DOWNLOAD_TIMEOUT).build();
    let mut buf = Vec::new();
    agent
        .get(url)
        .call()
        .with_context(|| format!("downloading {url}"))?
        .into_reader()
        .read_to_end(&mut buf)
        .context("reading the downloaded archive")?;
    Ok(buf)
}

/// Install `version` (a `vMAJOR.MINOR.PATCH` tag), replacing the running binary
/// with the prebuilt release for it.
///
/// The version is validated by [`is_valid_version_tag`] first; an invalid value
/// is refused outright. We then download the matching prebuilt archive, verify
/// its checksum, and swap the binary in place — no Rust toolchain required. The
/// running process keeps executing the old code until it is restarted.
pub fn install(version: &str) -> Result<()> {
    if !is_valid_version_tag(version) {
        bail!("refusing to update to {version:?}: not a vMAJOR.MINOR.PATCH version tag");
    }
    install_with(version, http_get, http_get_bytes)
}

/// The testable core of [`install`]: `fetch_manifest` and `download` are the
/// network seams. Resolves the artifact for this build's target, downloads and
/// verifies it, extracts the executable, and replaces the running binary.
fn install_with(
    version: &str,
    fetch_manifest: impl FnOnce(&str) -> Result<String>,
    download: impl FnOnce(&str) -> Result<Vec<u8>>,
) -> Result<()> {
    let url = manifest_url().context("no update manifest URL (unknown repository)")?;
    let manifest = fetch_manifest(&url)?;
    let artifact = resolve_artifact(&manifest, version, target_triple())?;
    let bytes = download_and_verify(&artifact.download_url, artifact.sha256.as_deref(), download)?;
    let exe = extract_executable(&bytes, &artifact.exe_name)?;
    replace_running_exe(&exe)
}

/// What [`install_with`] needs to fetch and unpack one platform's binary,
/// resolved from the manifest for a given target triple.
#[derive(Debug)]
struct ResolvedArtifact {
    download_url: String,
    /// Expected SHA-256 (hex) of the archive, if the manifest carries one.
    sha256: Option<String>,
    /// File name of the executable inside the archive (e.g. `mudpuppy` or
    /// `mudpuppy.exe`). Matched by basename, since dist nests it under a
    /// per-target directory.
    exe_name: String,
}

/// Locate, in `json`, the prebuilt archive for `triple` belonging to release
/// `version`. Errors if the manifest has moved past `version` (we only host the
/// latest manifest on Pages), names no release, or has no build for this platform.
fn resolve_artifact(json: &str, version: &str, triple: &str) -> Result<ResolvedArtifact> {
    let manifest: DistManifest =
        serde_json::from_str(json).context("parsing dist-manifest.json")?;

    // The Pages site only ever carries the latest release's manifest, so install
    // can only deliver that version. If a newer release landed between the check
    // and now, say so rather than silently installing something else.
    if let Some(tag) = manifest
        .announcement_tag
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        if normalize_tag(tag) != normalize_tag(version) {
            bail!(
                "the published manifest now advertises {}, not the requested {version}; re-run the update check",
                normalize_tag(tag)
            );
        }
    }

    let release = manifest
        .releases
        .iter()
        .find(|r| r.app_name == env!("CARGO_PKG_NAME"))
        .or_else(|| manifest.releases.first())
        .context("the manifest names no releases to install from")?;

    let github = release
        .hosting
        .github
        .as_ref()
        .context("the release carries no GitHub hosting info")?;

    let (name, artifact) = release
        .artifacts
        .iter()
        .filter_map(|n| manifest.artifacts.get(n).map(|a| (n, a)))
        .find(|(_, a)| a.kind == "executable-zip" && a.target_triples.iter().any(|t| t == triple))
        .with_context(|| {
            format!("the release has no prebuilt binary for this platform ({triple})")
        })?;

    let exe_name = artifact
        .assets
        .iter()
        .find(|a| a.kind == "executable")
        .map(|a| a.name.clone())
        .unwrap_or_else(|| env!("CARGO_PKG_NAME").to_string());

    let download_url = format!(
        "{}{}/{name}",
        github.artifact_base_url.trim_end_matches('/'),
        github.artifact_download_path,
    );

    Ok(ResolvedArtifact {
        download_url,
        sha256: artifact.checksums.get("sha256").cloned(),
        exe_name,
    })
}

/// Download the archive at `url` and, if `sha256` is given, reject it unless its
/// SHA-256 matches. `download` is the network seam mocked in tests.
fn download_and_verify(
    url: &str,
    sha256: Option<&str>,
    download: impl FnOnce(&str) -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    let bytes = download(url)?;
    if let Some(expected) = sha256 {
        use sha2::{Digest, Sha256};
        let got = hex_encode(&Sha256::digest(&bytes));
        if !got.eq_ignore_ascii_case(expected.trim()) {
            bail!("checksum mismatch for {url}: manifest expected {expected}, download was {got}");
        }
    }
    Ok(bytes)
}

/// Lowercase hex, to compare a computed digest against the manifest's checksum
/// string without pulling in a hex crate.
fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Extract the bytes of the executable named `exe_name` from a `.tar.xz` release
/// archive (macOS/Linux). Matched by file name so the per-target wrapper
/// directory dist adds doesn't matter.
#[cfg(not(windows))]
fn extract_executable(archive: &[u8], exe_name: &str) -> Result<Vec<u8>> {
    use std::io::{Cursor, Read};
    let mut decompressed = Vec::new();
    lzma_rs::xz_decompress(&mut Cursor::new(archive), &mut decompressed)
        .context("decompressing the .tar.xz release archive")?;
    let mut tar = tar::Archive::new(Cursor::new(decompressed));
    for entry in tar.entries().context("reading the release tarball")? {
        let mut entry = entry.context("reading a tarball entry")?;
        let is_match = entry
            .path()
            .ok()
            .and_then(|p| p.file_name().map(|f| f.to_str() == Some(exe_name)))
            .unwrap_or(false);
        if is_match {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .context("reading the binary from the tarball")?;
            return Ok(buf);
        }
    }
    bail!("the release archive did not contain an executable named {exe_name:?}");
}

/// Extract the bytes of the executable named `exe_name` from a `.zip` release
/// archive (Windows). Matched by file name, as for the tarball.
#[cfg(windows)]
fn extract_executable(archive: &[u8], exe_name: &str) -> Result<Vec<u8>> {
    use std::io::{Cursor, Read};
    let mut zip =
        zip::ZipArchive::new(Cursor::new(archive)).context("opening the .zip release archive")?;
    for i in 0..zip.len() {
        let mut file = zip.by_index(i).context("reading a zip entry")?;
        let is_match = std::path::Path::new(file.name())
            .file_name()
            .and_then(|f| f.to_str())
            .map(|f| f == exe_name)
            .unwrap_or(false);
        if is_match {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)
                .context("reading the binary from the zip")?;
            return Ok(buf);
        }
    }
    bail!("the release archive did not contain an executable named {exe_name:?}");
}

/// Replace the running executable with `exe_bytes`. Stages the new binary beside
/// the current one (so the swap is a same-filesystem operation), marks it
/// executable on Unix, then hands off to `self_replace`, which knows how to
/// replace a *running* binary on each platform.
fn replace_running_exe(exe_bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let current = std::env::current_exe().context("locating the running executable")?;
    let dir = current
        .parent()
        .context("the running executable has no parent directory")?;

    let mut staged =
        tempfile::NamedTempFile::new_in(dir).context("staging the downloaded binary")?;
    staged
        .write_all(exe_bytes)
        .context("writing the downloaded binary")?;
    staged.flush().context("flushing the downloaded binary")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(staged.path(), std::fs::Permissions::from_mode(0o755))
            .context("making the new binary executable")?;
    }

    let (_file, staged_path) = staged.keep().context("persisting the staged binary")?;
    let result = self_replace::self_replace(&staged_path)
        .context("replacing the running executable with the update");
    // self_replace copies the staged file into place; remove our staging copy
    // either way (ignore failure — it's a temp file, and the update has landed).
    let _ = std::fs::remove_file(&staged_path);
    result
}

/// Whether `tag` is a release tag we will pass to [`install`]: exactly
/// `v<digits>.<digits>.<digits>`. Rejects anything with extra components, missing
/// parts, non-digits, or stray characters (slashes, path traversal, ranges) — the
/// input gate that keeps a version value, even one from the network, from steering
/// the update toward an unintended download.
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
    fn install_refuses_invalid_versions_before_any_network() {
        // The guard fires before any fetch/download; the error names the offender.
        let err = install("v1.2.3; rm -rf /").unwrap_err();
        assert!(err.to_string().contains("refusing to update"));
    }

    /// A manifest with one darwin (.tar.xz) and one windows (.zip) prebuilt
    /// artifact, mirroring the real dist-manifest.json shape we resolve against.
    fn manifest_with_artifacts(tag: &str) -> String {
        format!(
            r#"{{
              "announcement_tag": "{tag}",
              "releases": [{{
                "app_name": "mudpuppy",
                "app_version": "1.2.3",
                "artifacts": ["mudpuppy-aarch64-apple-darwin.tar.xz", "mudpuppy-x86_64-pc-windows-msvc.zip"],
                "hosting": {{ "github": {{
                  "artifact_base_url": "https://github.com",
                  "artifact_download_path": "/o/mudpuppy/releases/download/{tag}"
                }} }}
              }}],
              "artifacts": {{
                "mudpuppy-aarch64-apple-darwin.tar.xz": {{
                  "kind": "executable-zip",
                  "target_triples": ["aarch64-apple-darwin"],
                  "assets": [{{ "name": "mudpuppy", "kind": "executable" }}],
                  "checksums": {{ "sha256": "abc123" }}
                }},
                "mudpuppy-x86_64-pc-windows-msvc.zip": {{
                  "kind": "executable-zip",
                  "target_triples": ["x86_64-pc-windows-msvc"],
                  "assets": [{{ "name": "mudpuppy.exe", "kind": "executable" }}],
                  "checksums": {{ "sha256": "def456" }}
                }}
              }}
            }}"#
        )
    }

    #[test]
    fn resolve_artifact_builds_the_download_url_for_a_target() {
        let json = manifest_with_artifacts("v1.2.3");
        let darwin = resolve_artifact(&json, "v1.2.3", "aarch64-apple-darwin").unwrap();
        assert_eq!(
            darwin.download_url,
            "https://github.com/o/mudpuppy/releases/download/v1.2.3/mudpuppy-aarch64-apple-darwin.tar.xz"
        );
        assert_eq!(darwin.sha256.as_deref(), Some("abc123"));
        assert_eq!(darwin.exe_name, "mudpuppy");

        // A different triple selects a different artifact (and exe name).
        let win = resolve_artifact(&json, "v1.2.3", "x86_64-pc-windows-msvc").unwrap();
        assert!(win
            .download_url
            .ends_with("mudpuppy-x86_64-pc-windows-msvc.zip"));
        assert_eq!(win.exe_name, "mudpuppy.exe");
    }

    #[test]
    fn resolve_artifact_errors_when_no_build_for_platform() {
        let json = manifest_with_artifacts("v1.2.3");
        let err = resolve_artifact(&json, "v1.2.3", "sparc-unknown-none").unwrap_err();
        assert!(err.to_string().contains("no prebuilt binary"), "{err}");
    }

    #[test]
    fn resolve_artifact_errors_when_manifest_moved_past_requested_version() {
        // The Pages manifest advertises a newer release than the one we were asked
        // to install — refuse rather than install the wrong thing.
        let json = manifest_with_artifacts("v1.2.4");
        let err = resolve_artifact(&json, "v1.2.3", "aarch64-apple-darwin").unwrap_err();
        assert!(err.to_string().contains("now advertises v1.2.4"), "{err}");
    }

    #[test]
    fn download_and_verify_accepts_a_matching_checksum() {
        use sha2::{Digest, Sha256};
        let body = b"prebuilt-archive-bytes".to_vec();
        let sum = hex_encode(&Sha256::digest(&body));
        let got = download_and_verify("http://x", Some(&sum), |_| Ok(body.clone())).unwrap();
        assert_eq!(got, body);
    }

    #[test]
    fn download_and_verify_rejects_a_mismatched_checksum() {
        let err = download_and_verify("http://x", Some("deadbeef"), |_| Ok(b"bytes".to_vec()))
            .unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
    }

    /// Round-trip the platform archive format: pack a fake binary under a wrapper
    /// directory (as dist does) and confirm extraction finds it by basename.
    #[test]
    #[cfg(not(windows))]
    fn extract_executable_pulls_the_binary_from_a_tar_xz() {
        use std::io::Cursor;
        let payload = b"#!/fake/elf\x00binary".to_vec();
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            let mut header = tar::Header::new_gnu();
            header.set_size(payload.len() as u64);
            header.set_mode(0o755);
            builder
                .append_data(&mut header, "mudpuppy-x/mudpuppy", payload.as_slice())
                .unwrap();
            builder.finish().unwrap();
        }
        let mut xz = Vec::new();
        lzma_rs::xz_compress(&mut Cursor::new(&tar_bytes), &mut xz).unwrap();

        assert_eq!(extract_executable(&xz, "mudpuppy").unwrap(), payload);
        assert!(extract_executable(&xz, "other").is_err());
    }

    #[test]
    #[cfg(windows)]
    fn extract_executable_pulls_the_binary_from_a_zip() {
        use std::io::{Cursor, Write};
        let payload = b"MZ\x90\x00fake-pe".to_vec();
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zw.start_file("mudpuppy-x/mudpuppy.exe", opts).unwrap();
            zw.write_all(&payload).unwrap();
            zw.finish().unwrap();
        }
        assert_eq!(extract_executable(&buf, "mudpuppy.exe").unwrap(), payload);
        assert!(extract_executable(&buf, "other.exe").is_err());
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
        assert_eq!(newer.map(|r| r.version).as_deref(), Some("v9.9.9"));

        // The running version itself → nothing to offer.
        let same = check_with(|_url| Ok(manifest_with_tag(current_version()))).unwrap();
        assert!(same.is_none());

        // An older version → nothing to offer.
        let older = check_with(|_url| Ok(manifest_with_tag("v0.0.0"))).unwrap();
        assert!(older.is_none());
    }

    #[test]
    fn check_with_surfaces_the_changelog() {
        // Embed an escaped newline in the changelog the way the real manifest does.
        let json = "{ \"announcement_tag\": \"v9.9.9\", \
             \"announcement_changelog\": \"### Added\\n- a shiny thing\\n\", \
             \"releases\": [] }";
        let info = check_with(|_url| Ok(json.to_string())).unwrap().unwrap();
        assert_eq!(info.version, "v9.9.9");
        // Trimmed, but the body is preserved for the prompt to render.
        let changelog = info.changelog.expect("changelog present");
        assert!(changelog.contains("a shiny thing"));
        assert!(!changelog.ends_with('\n'), "trailing whitespace trimmed");
    }

    #[test]
    fn release_from_manifest_drops_an_empty_changelog() {
        let json =
            "{ \"announcement_tag\": \"v9.9.9\", \"announcement_changelog\": \"  \\n \", \"releases\": [] }";
        let info = release_from_manifest(json).unwrap().unwrap();
        assert!(info.changelog.is_none());
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

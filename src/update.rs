//! `fuckmemory update` — self-update from a GitHub release asset.
//!
//! The release workflow uploads `fuckmemory-<target>.tar.gz` (unix) or
//! `fuckmemory-<target>.zip` (windows) per release. This command queries the
//! GitHub API for the latest release, picks the asset that matches the running
//! binary's platform, downloads it, and atomically replaces the running binary —
//! no shell, no manual `curl | sh`, no touching the install directory by hand.
//!
//! Two safety rails apply everywhere:
//!
//! - **Never download over an unknown version.** We only replace the binary when
//!   the remote tag is strictly newer, unless `--force` says otherwise.
//! - **The old binary stays until the new one is fully written.** We download to
//!   a temp file, extract into a second temp file next to the target, then rename
//!   over it — so a crash can't leave a half-written executable behind.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::Value;

/// Where releases are published. Matches the `repository` field in Cargo.toml.
const REPO: &str = "dalsori/fuckmemory";
const LATEST_API: &str = "https://api.github.com/repos/dalsori/fuckmemory/releases/latest";

/// Outcome of a check, so the CLI can say more than "old/new".
pub struct Check {
    pub current: String,
    pub latest: String,
    pub asset_name: String,
    pub update_available: bool,
}

/// Parse a `v1.2.3` (or `1.2.3`) tag into comparable numbers.
fn version_ints(v: &str) -> Option<Vec<u64>> {
    v.trim()
        .trim_start_matches('v')
        .split('.')
        .map(|part| part.trim().parse().ok())
        .collect()
}

/// Strict semver-ish ordering: `1.10.0 > 1.9.0`, missing parts count as 0.
fn compare_versions(a: &str, b: &str) -> Ordering {
    let mut ai = version_ints(a).unwrap_or_default();
    let mut bi = version_ints(b).unwrap_or_default();
    while ai.len() < bi.len() {
        ai.push(0);
    }
    while bi.len() < ai.len() {
        bi.push(0);
    }
    ai.cmp(&bi)
}

/// The release asset name for this platform, or `None` when we don't know it.
///
/// Must stay in lockstep with `.github/workflows/release.yml`'s `matrix`.
fn asset_name() -> Option<String> {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        _ => return None,
    };
    let os = match std::env::consts::OS {
        "linux" => format!("{arch}-unknown-linux-gnu"),
        "macos" => format!("{arch}-apple-darwin"),
        "windows" => format!("{arch}-pc-windows-msvc"),
        _ => return None,
    };
    Some(if std::env::consts::OS == "windows" {
        format!("fuckmemory-{os}.zip")
    } else {
        format!("fuckmemory-{os}.tar.gz")
    })
}

fn http() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(15))
        .timeout_read(std::time::Duration::from_secs(300))
        .build()
}

/// Query GitHub for the latest release and its matching asset.
pub fn latest_release() -> Result<Check> {
    let Some(asset_name) = asset_name() else {
        bail!(
            "no release asset for this platform ({} / {})",
            std::env::consts::ARCH,
            std::env::consts::OS
        );
    };
    let body = http()
        .get(LATEST_API)
        .set("User-Agent", REPO)
        .set("Accept", "application/vnd.github+json")
        .call()
        .context("checking the GitHub API — are you online?")?
        .into_string()
        .context("reading the GitHub API response")?;
    let v: Value = serde_json::from_str(&body).context("parsing the GitHub API response")?;
    let latest = v
        .get("tag_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let has_asset = v
        .get("assets")
        .and_then(Value::as_array)
        .map(|assets| {
            assets
                .iter()
                .any(|a| a.get("name").and_then(Value::as_str) == Some(asset_name.as_str()))
        })
        .unwrap_or(false);
    if latest.is_empty() {
        bail!("GitHub returned no tag_name — unexpected response");
    }
    let current = env!("CARGO_PKG_VERSION");
    let update_available = has_asset && compare_versions(&latest, current) == Ordering::Greater;
    Ok(Check {
        current: current.to_string(),
        latest,
        asset_name,
        update_available,
    })
}

fn download(url: &str, dest: &Path) -> Result<()> {
    let resp = http()
        .get(url)
        .set("User-Agent", REPO)
        .call()
        .with_context(|| format!("downloading {url}"))?;
    let mut body = resp.into_reader();
    let tmp = dest.with_extension("fuckmemory-dl");
    {
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        std::io::copy(&mut body, &mut f).with_context(|| format!("writing {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, dest).context("finalizing the download")?;
    Ok(())
}

/// Extract the single `fuckmemory` (or `.exe`) binary from an archive.
fn extract_archive(archive: &Path, dest: &Path) -> Result<()> {
    extract_archive_impl(archive, dest)
}

#[cfg(unix)]
fn extract_archive_impl(archive: &Path, dest: &Path) -> Result<()> {
    let file =
        std::fs::File::open(archive).with_context(|| format!("opening {}", archive.display()))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(gz);
    let mut found = false;
    for entry in tar.entries().context("reading the tar archive")? {
        let mut entry = entry.context("reading a tar entry")?;
        let path = entry.path().context("reading a tar path")?.into_owned();
        if path.ends_with("fuckmemory") || path.ends_with("fuckmemory.exe") {
            let mut out = std::fs::File::create(dest)?;
            std::io::copy(&mut entry, &mut out)?;
            found = true;
        }
    }
    if !found {
        bail!("no fuckmemory binary in {}", archive.display());
    }
    Ok(())
}

#[cfg(windows)]
fn extract_archive_impl(archive: &Path, dest: &Path) -> Result<()> {
    let file =
        std::fs::File::open(archive).with_context(|| format!("opening {}", archive.display()))?;
    let mut zip =
        zip::ZipArchive::new(file).with_context(|| format!("reading {}", archive.display()))?;
    let names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
        .collect();
    let member = names
        .iter()
        .find(|n| n.ends_with("fuckmemory.exe"))
        .or_else(|| names.iter().find(|n| n.contains("fuckmemory")))
        .with_context(|| format!("no fuckmemory.exe in {}", archive.display()))?;
    let mut member = zip
        .by_name(member)
        .with_context(|| format!("opening {member} in the archive"))?;
    let mut out = std::fs::File::create(dest)?;
    std::io::copy(&mut member, &mut out)?;
    Ok(())
}

/// Apply the update: download the asset, extract, swap the binary in place.
///
/// `current_exe` is injected so tests can point at a throwaway path.
pub fn apply(check: &Check, current_exe: &Path) -> Result<PathBuf> {
    let asset_url = format!(
        "https://github.com/{REPO}/releases/download/{}/{}",
        check.latest, check.asset_name
    );
    let tmp_dir = std::env::temp_dir().join(format!("fuckmemory-update-{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).context("creating a temp dir for the download")?;
    let archive = tmp_dir.join(&check.asset_name);
    download(&asset_url, &archive)?;

    // Extract next to the target, then rename over it: the rename is atomic and
    // keeps a running server on the old inode until the switch.
    let staged = current_exe.with_extension(format!("new-{}", std::process::id()));
    extract_archive(&archive, &staged)?;
    make_executable(&staged);
    replace_binary(&staged, current_exe)?;

    let _ = std::fs::remove_dir_all(&tmp_dir);
    Ok(current_exe.to_path_buf())
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
}

#[cfg(windows)]
fn make_executable(_path: &Path) {}

/// Swap `staged` over `target`. On unix a plain rename works; on Windows the
/// running binary cannot be replaced in place, so we try rename and fall back to
/// a copy.
#[cfg(unix)]
fn replace_binary(staged: &Path, target: &Path) -> Result<()> {
    std::fs::rename(staged, target)
        .with_context(|| format!("replacing {} — is the file writable?", target.display()))
}

#[cfg(windows)]
fn replace_binary(staged: &Path, target: &Path) -> Result<()> {
    // Windows refuses to overwrite or delete a *running* executable, but it
    // allows renaming it. So the self-update dance is: move the current binary
    // aside, slide the new one into place, and let the old file be reclaimed
    // once the process finally exits — the next update renames over it. The old
    // file stays on disk until the swap completes, so a crash mid-update can
    // never leave the install without a binary.
    let old = target.with_extension("old.exe");
    let _ = std::fs::remove_file(&old);
    if std::fs::rename(target, &old).is_ok() {
        if std::fs::rename(staged, target).is_ok() {
            // Fails while this process is still running from `old`; that is
            // expected and harmless — the next update replaces it.
            let _ = std::fs::remove_file(&old);
            return Ok(());
        }
        // The swap failed; put the old binary back so the install is intact.
        let _ = std::fs::rename(&old, target);
        let _ = std::fs::remove_file(staged);
        bail!(
            "replacing {} — the binary was in use; close running sessions and retry",
            target.display()
        );
    }
    // The target wasn't locked: plain rename, then copy as a last resort.
    if std::fs::rename(staged, target).is_ok() {
        return Ok(());
    }
    std::fs::copy(staged, target).with_context(|| {
        format!(
            "replacing {} — try closing running sessions",
            target.display()
        )
    })?;
    let _ = std::fs::remove_file(staged);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::io::Read;

    #[test]
    fn compares_versions_semverishly() {
        assert_eq!(compare_versions("1.2.3", "1.2.3"), Ordering::Equal);
        assert_eq!(compare_versions("1.2.3", "1.2.4"), Ordering::Less);
        assert_eq!(compare_versions("1.10.0", "1.9.0"), Ordering::Greater);
        assert_eq!(compare_versions("v2.0.0", "1.9.9"), Ordering::Greater);
        assert_eq!(compare_versions("1.2", "1.2.0"), Ordering::Equal);
    }

    #[test]
    fn asset_names_match_the_release_matrix() {
        let expected = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", "x86_64") => "fuckmemory-x86_64-unknown-linux-gnu.tar.gz",
            ("linux", "aarch64") => "fuckmemory-aarch64-unknown-linux-gnu.tar.gz",
            ("macos", "x86_64") => "fuckmemory-x86_64-apple-darwin.tar.gz",
            ("macos", "aarch64") => "fuckmemory-aarch64-apple-darwin.tar.gz",
            ("windows", "x86_64") => "fuckmemory-x86_64-pc-windows-msvc.zip",
            ("windows", "aarch64") => "fuckmemory-aarch64-pc-windows-msvc.zip",
            _ => {
                assert!(
                    asset_name().is_none(),
                    "unknown platform should have no asset"
                );
                return;
            }
        };
        assert_eq!(asset_name().as_deref(), Some(expected));
    }

    #[cfg(unix)]
    #[test]
    fn extracts_the_binary_from_a_release_archive() {
        let dir = std::env::temp_dir().join(format!("fm-upd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("fuckmemory-x86_64-unknown-linux-gnu.tar.gz");
        let fake = dir.join("fuckmemory");
        std::fs::write(&fake, b"#!/bin/sh\necho fake\n").unwrap();

        // Build a real tar.gz with flate2+tar, then run the extractor on it.
        let tar_path = dir.join("fuckmemory.tar");
        {
            let file = std::fs::File::create(&tar_path).unwrap();
            let mut tar = tar::Builder::new(file);
            tar.append_path_with_name(&fake, "fuckmemory").unwrap();
            tar.finish().unwrap();
        }
        {
            let tar_in = std::fs::File::open(&tar_path).unwrap();
            let len = tar_in.metadata().unwrap().len();
            let mut gz = flate2::write::GzEncoder::new(
                std::fs::File::create(&archive).unwrap(),
                flate2::Compression::default(),
            );
            std::io::copy(&mut tar_in.take(len), &mut gz).unwrap();
            gz.finish().unwrap();
        }

        let dest = dir.join("out");
        extract_archive(&archive, &dest).unwrap();
        assert!(dest.exists(), "binary should be extracted");
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            "#!/bin/sh\necho fake\n"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}

//! Icon cache, `icons.lock` and network fetch, shared by `guicons-build`'s
//! codegen, `guicons-macros`' `icon!` and the CLI. All of them resolve the
//! cache from the same manifest, so a `icons fetch` and a later build agree
//! on where an icon lives and what it must hash to.

use guicons_core::IconManifest;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const ALLOW_NETWORK_ENV: &str = "GUICONS_ALLOW_NETWORK";

/// Written next to the manifest; commit it together with the cache.
pub const LOCK_FILE: &str = "icons.lock";

const MAX_ICON_BYTES: u64 = 1024 * 1024;

#[derive(Debug)]
pub struct DownloadError(String);

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DownloadError {}

fn error(message: impl Into<String>) -> DownloadError {
    DownloadError(message.into())
}

/// `.cache/guicons` under the workspace root of the (already resolved)
/// manifest.
pub fn cache_dir(manifest: &IconManifest) -> PathBuf {
    manifest.workspace_root().join(".cache").join("guicons")
}

pub fn lock_path(manifest: &IconManifest) -> PathBuf {
    manifest.manifest_path().with_file_name(LOCK_FILE)
}

/// An icon that lives on the network rather than in the source tree.
#[derive(Clone, Copy, Debug)]
pub enum RemoteIcon<'a> {
    Iconify(&'a str),
    Url(&'a str),
}

impl RemoteIcon<'_> {
    pub fn label(&self) -> &str {
        match self {
            Self::Iconify(id) | Self::Url(id) => id,
        }
    }

    pub fn lock_key(&self) -> String {
        match self {
            Self::Iconify(id) => format!("iconify:{id}"),
            Self::Url(url) => format!("url:{url}"),
        }
    }

    pub fn cache_path(&self, cache_dir: &Path) -> Result<PathBuf, DownloadError> {
        match self {
            Self::Iconify(id) => {
                let (provider, name) = split_iconify_id(id)?;
                Ok(cache_dir.join(provider).join(format!("{name}.svg")))
            }
            Self::Url(url) => Ok(cache_dir.join("url").join(format!("{}.svg", sha256_hex(url.as_bytes())))),
        }
    }

    pub fn url(&self) -> Result<String, DownloadError> {
        match self {
            Self::Iconify(id) => {
                let (provider, name) = split_iconify_id(id)?;
                Ok(format!("https://api.iconify.design/{provider}/{name}.svg"))
            }
            Self::Url(url) => Ok(url.to_string()),
        }
    }
}

/// `sha256  key` per line, sorted, like `sha256sum` output.
#[derive(Debug, Default)]
pub struct Lock {
    entries: BTreeMap<String, String>,
}

impl Lock {
    /// An empty lock if the file doesn't exist.
    pub fn load(path: &Path) -> Result<Self, DownloadError> {
        let content = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(error(format!("Failed to read {}: {e}", path.display()))),
        };
        let mut entries = BTreeMap::new();
        for (index, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (hash, key) = line
                .split_once("  ")
                .filter(|(hash, key)| is_sha256_hex(hash) && !key.is_empty())
                .ok_or_else(|| error(format!("{}:{}: expected `<sha256>  <icon>`", path.display(), index + 1)))?;
            entries.insert(key.to_string(), hash.to_string());
        }
        Ok(Self { entries })
    }

    pub fn get(&self, icon: RemoteIcon<'_>) -> Option<&str> {
        self.entries.get(&icon.lock_key()).map(String::as_str)
    }

    pub fn set(&mut self, icon: RemoteIcon<'_>, sha256: String) {
        self.entries.insert(icon.lock_key(), sha256);
    }

    pub fn retain(&mut self, icons: &[RemoteIcon<'_>]) {
        let keys: Vec<String> = icons.iter().map(RemoteIcon::lock_key).collect();
        self.entries.retain(|key, _| keys.contains(key));
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn render(&self) -> String {
        let mut out = String::from("# sha256 of each downloaded icon, written by `icons fetch`; commit it with the cache.\n");
        for (key, hash) in &self.entries {
            out.push_str(&format!("{hash}  {key}\n"));
        }
        out
    }

    pub fn save(&self, path: &Path) -> Result<(), DownloadError> {
        write_atomic(path, self.render().as_bytes())
    }

    /// An error if `icon` is locked to a different hash than `bytes`.
    pub fn verify(&self, icon: RemoteIcon<'_>, bytes: &[u8]) -> Result<(), DownloadError> {
        match self.get(icon) {
            Some(expected) if expected != sha256_hex(bytes) => Err(error(format!(
                "`{}` doesn't match {LOCK_FILE}: sha256 {}, locked {expected}",
                icon.label(),
                sha256_hex(bytes)
            ))),
            _ => Ok(()),
        }
    }
}

/// The cached copy of `icon`, downloading it first when it's missing and
/// `GUICONS_ALLOW_NETWORK` is set. Checked against `icons.lock` either way.
pub fn ensure_cached(manifest: &IconManifest, icon: RemoteIcon<'_>) -> Result<PathBuf, DownloadError> {
    ensure_cached_at(&cache_dir(manifest), &lock_path(manifest), icon)
}

/// [`ensure_cached`] for a crate without a manifest, with the cache and
/// `icons.lock` at `workspace_root`.
pub fn ensure_cached_in_workspace(workspace_root: &Path, icon: RemoteIcon<'_>) -> Result<PathBuf, DownloadError> {
    ensure_cached_at(&workspace_root.join(".cache").join("guicons"), &workspace_root.join(LOCK_FILE), icon)
}

fn ensure_cached_at(cache_dir: &Path, lock_path: &Path, icon: RemoteIcon<'_>) -> Result<PathBuf, DownloadError> {
    let path = icon.cache_path(cache_dir)?;
    let lock = Lock::load(lock_path)?;
    if let Ok(bytes) = fs::read(&path) {
        lock.verify(icon, &bytes)?;
        return Ok(path);
    }
    if env::var_os(ALLOW_NETWORK_ENV).is_none() {
        return Err(error(format!(
            "Icon `{}` is missing from cache at {}. Run `icons fetch` or set {ALLOW_NETWORK_ENV}=1.",
            icon.label(),
            path.display()
        )));
    }
    let bytes = fetch(icon)?;
    lock.verify(icon, &bytes)?;
    write_atomic(&path, &bytes)?;
    Ok(path)
}

/// Downloads `icon` and checks it is an SVG or PNG under the size limit.
pub fn fetch(icon: RemoteIcon<'_>) -> Result<Vec<u8>, DownloadError> {
    let url = icon.url()?;
    check_scheme(&url)?;
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .build();
    let response = agent
        .get(&url)
        .call()
        .map_err(|e| error(format!("Failed to download `{url}`: {e}")))?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_ICON_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| error(format!("Failed to read `{url}`: {e}")))?;
    if bytes.len() as u64 > MAX_ICON_BYTES {
        return Err(error(format!("`{url}` is larger than {MAX_ICON_BYTES} bytes")));
    }
    if !is_icon_image(&bytes) {
        return Err(error(format!("`{url}` returned something that is neither SVG nor PNG")));
    }
    Ok(bytes)
}

/// Writes through a temporary file in the same directory and renames it
/// into place.
pub fn write_atomic(dest: &Path, bytes: &[u8]) -> Result<(), DownloadError> {
    let dir = dest.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir).map_err(|e| error(format!("Failed to create {}: {e}", dir.display())))?;
    let mut file = tempfile::NamedTempFile::new_in(dir)
        .map_err(|e| error(format!("Failed to create a temporary file in {}: {e}", dir.display())))?;
    std::io::Write::write_all(&mut file, bytes)
        .map_err(|e| error(format!("Failed to write {}: {e}", dest.display())))?;
    file.persist(dest).map_err(|e| error(format!("Failed to write {}: {e}", dest.display())))?;
    Ok(())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn split_iconify_id(id: &str) -> Result<(&str, &str), DownloadError> {
    let valid = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    match id.split_once(':') {
        Some((provider, name)) if valid(provider) && valid(name) => Ok((provider, name)),
        _ => Err(error(format!("iconify id must be `<set>:<name>` in lowercase letters, digits and `-`, got `{id}`"))),
    }
}

fn check_scheme(url: &str) -> Result<(), DownloadError> {
    if url.starts_with("https://") {
        return Ok(());
    }
    let local = ["http://localhost", "http://127.0.0.1", "http://[::1]"]
        .iter()
        .any(|prefix| url.strip_prefix(prefix).is_some_and(|rest| rest.is_empty() || rest.starts_with([':', '/'])));
    if local {
        Ok(())
    } else {
        Err(error(format!("`{url}` must use https://")))
    }
}

fn is_icon_image(bytes: &[u8]) -> bool {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return true;
    }
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    let text = text.trim_start_matches('\u{feff}').trim_start();
    text.starts_with('<') && text.contains("<svg") && !text.contains("<html")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iconify_ids_that_escape_the_cache_are_rejected() {
        for id in ["home", "mdi:home.svg#/../../evil", "a:b:c", "MDI:home", "mdi:"] {
            assert!(RemoteIcon::Iconify(id).cache_path(Path::new("cache")).is_err(), "{id}");
        }
        assert_eq!(
            RemoteIcon::Iconify("mdi:home-2").cache_path(Path::new("cache")).unwrap(),
            Path::new("cache").join("mdi").join("home-2.svg")
        );
    }

    #[test]
    fn only_https_or_loopback_http_is_fetched() {
        assert!(check_scheme("https://example.com/a.svg").is_ok());
        assert!(check_scheme("http://127.0.0.1:8080/a.svg").is_ok());
        assert!(check_scheme("http://localhost/a.svg").is_ok());
        assert!(check_scheme("http://example.com/a.svg").is_err());
        assert!(check_scheme("http://localhost.example.com/a.svg").is_err());
        assert!(check_scheme("file:///etc/passwd").is_err());
    }

    #[test]
    fn html_is_not_an_icon() {
        assert!(is_icon_image(b"\xef\xbb\xbf  <?xml version=\"1.0\"?>\n<svg/>"));
        assert!(is_icon_image(b"\x89PNG\r\n\x1a\n...."));
        assert!(!is_icon_image(b"<!DOCTYPE html><html><body><svg/></body></html>"));
        assert!(!is_icon_image(b"{\"error\": 404}"));
    }

    #[test]
    fn lock_round_trips_and_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOCK_FILE);
        let home = RemoteIcon::Iconify("mdi:home");
        let mut lock = Lock::default();
        lock.set(home, sha256_hex(b"<svg/>"));
        lock.save(&path).unwrap();

        let lock = Lock::load(&path).unwrap();
        assert!(lock.verify(home, b"<svg/>").is_ok());
        assert!(lock.verify(home, b"<svg>changed</svg>").is_err());
        assert!(lock.verify(RemoteIcon::Url("https://example.com/a.svg"), b"anything").is_ok());
    }

    #[test]
    fn malformed_lock_line_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOCK_FILE);
        fs::write(&path, "not-a-hash  iconify:mdi:home\n").unwrap();
        assert!(Lock::load(&path).is_err());
    }
}

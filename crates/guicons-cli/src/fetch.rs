use guicons_core::IconEntrySource;
use guicons_net::{Lock, RemoteIcon};
use std::fs;
use std::path::Path;

#[derive(Debug)]
pub struct FetchSummary {
    pub fetched: Vec<String>,
    pub skipped: Vec<String>,
    pub failed: Vec<(String, String)>,
}

impl FetchSummary {
    pub fn is_success(&self) -> bool {
        self.failed.is_empty()
    }
}

/// Populates the manifest's cache directory ([`guicons_net::cache_dir`]) for
/// every `iconify`/`url` entry and records their sha256 in `icons.lock`.
///
/// A download or cached copy that differs from the lock fails that icon;
/// `force` re-downloads everything and re-locks it to what came back.
pub fn fetch(manifest_path: &Path, force: bool) -> Result<FetchSummary, Vec<String>> {
    let (manifest, errors) = guicons_core::load_icon_manifest(manifest_path);
    if !errors.is_empty() {
        return Err(errors.iter().map(|e| e.to_string()).collect());
    }
    let cache_dir = guicons_net::cache_dir(&manifest);
    let lock_path = guicons_net::lock_path(&manifest);
    let mut lock = Lock::load(&lock_path).map_err(|e| vec![e.to_string()])?;

    let mut summary = FetchSummary {
        fetched: Vec::new(),
        skipped: Vec::new(),
        failed: Vec::new(),
    };

    let icons: Vec<RemoteIcon<'_>> = manifest
        .entries()
        .iter()
        .filter_map(|entry| match entry.source() {
            IconEntrySource::Iconify(id) => Some(RemoteIcon::Iconify(id)),
            IconEntrySource::Url(url) => Some(RemoteIcon::Url(url)),
            IconEntrySource::File(_) | IconEntrySource::Glyph(_) => None,
        })
        .collect();

    for &icon in &icons {
        let label = icon.label().to_string();
        match fetch_one(icon, &cache_dir, &lock, force) {
            Ok((sha256, downloaded)) => {
                lock.set(icon, sha256);
                if downloaded {
                    summary.fetched.push(label);
                } else {
                    summary.skipped.push(label);
                }
            }
            Err(e) => summary.failed.push((label, e.to_string())),
        }
    }

    lock.retain(&icons);
    if !lock.is_empty() || lock_path.exists() {
        lock.save(&lock_path).map_err(|e| vec![e.to_string()])?;
    }
    Ok(summary)
}

fn fetch_one(
    icon: RemoteIcon<'_>,
    cache_dir: &Path,
    lock: &Lock,
    force: bool,
) -> Result<(String, bool), guicons_net::DownloadError> {
    let cache_path = icon.cache_path(cache_dir)?;
    if !force && let Ok(bytes) = fs::read(&cache_path) {
        lock.verify(icon, &bytes)?;
        return Ok((guicons_net::sha256_hex(&bytes), false));
    }
    let bytes = guicons_net::fetch(icon)?;
    if !force {
        lock.verify(icon, &bytes)?;
    }
    guicons_net::write_atomic(&cache_path, &bytes)?;
    Ok((guicons_net::sha256_hex(&bytes), true))
}

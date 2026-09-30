use guicons_core::{IconEntrySource, IconManifest};
use guicons_net::{Lock, RemoteIcon};
use miette::{LabeledSpan, MietteDiagnostic, NamedSource, Report, Severity};
use std::collections::HashMap;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

/// Loads `manifest_path` and turns every [`guicons_core::ManifestError`]
/// (real errors - manifest *shape* is wrong) plus two filesystem-level
/// checks `guicons_core` itself deliberately doesn't do (it only validates
/// shape, not what fields point to) into pretty-printable [`miette::Report`]s:
///
/// - a `file` source pointing at a path that doesn't exist - a real error,
///   since nothing will ever be able to load it.
/// - an `iconify`/`url` source with no cached copy on disk yet - only
///   *advice*, since resolving it for real needs network access
///   (`icons fetch`) that `check` deliberately never does itself.
/// - a cached copy that doesn't match `icons.lock` - an error; one that
///   isn't in an existing `icons.lock` yet - advice.
///
/// `windows-ico` paths are not checked.
///
/// Returns the number of entries that parsed successfully alongside the
/// reports (empty if the manifest is fully valid, though it may still
/// contain advice-level notes - see [`Severity`] on each report).
pub fn check(manifest_path: &Path) -> (usize, Vec<Report>) {
    let (manifest, errors) = guicons_core::load_icon_manifest(manifest_path);
    let mut source_cache: HashMap<PathBuf, String> = HashMap::new();

    let mut reports: Vec<Report> = errors
        .iter()
        .map(|error| build_report(&error.file, error.span.clone(), error.message.clone(), Severity::Error, &mut source_cache))
        .collect();
    let lock = load_lock(&manifest);

    for entry in manifest.entries() {
        match entry.source() {
            IconEntrySource::File(path) if !path.exists() => {
                reports.push(build_report(
                    entry.file(),
                    Some(entry.span()),
                    format!(
                        "icon manifest entry `{}` has a `file` source that doesn't exist: `{}`",
                        entry.key(),
                        display_path(path)
                    ),
                    Severity::Error,
                    &mut source_cache,
                ));
            }
            IconEntrySource::Iconify(id) => {
                if let Some((message, severity)) = remote_problem(&manifest, &lock, RemoteIcon::Iconify(id)) {
                    reports.push(build_report(entry.file(), Some(entry.span()), message, severity, &mut source_cache));
                }
            }
            IconEntrySource::Url(url) => {
                if let Some((message, severity)) = remote_problem(&manifest, &lock, RemoteIcon::Url(url)) {
                    reports.push(build_report(entry.file(), Some(entry.span()), message, severity, &mut source_cache));
                }
            }
            _ => {}
        }
    }

    (manifest.entries().len(), reports)
}

enum LockState {
    Missing,
    Present(Lock),
    Unreadable(String),
}

fn load_lock(manifest: &IconManifest) -> LockState {
    let path = guicons_net::lock_path(manifest);
    if !path.exists() {
        return LockState::Missing;
    }
    match Lock::load(&path) {
        Ok(lock) => LockState::Present(lock),
        Err(e) => LockState::Unreadable(e.to_string()),
    }
}

/// Why a remote icon can't be confirmed from the cache and `icons.lock`,
/// or `None` if it can.
fn remote_problem(manifest: &IconManifest, lock: &LockState, icon: RemoteIcon<'_>) -> Option<(String, Severity)> {
    let label = icon.label();
    let cache_path = match icon.cache_path(&guicons_net::cache_dir(manifest)) {
        Ok(path) => path,
        Err(e) => return Some((format!("{e} - it will never resolve"), Severity::Advice)),
    };
    let Ok(bytes) = fs::read(&cache_path) else {
        return Some((
            format!(
                "`{label}` isn't cached locally yet, so `check` can't confirm it resolves - run `icons fetch` (or set `GUICONS_ALLOW_NETWORK=1`) to fetch and verify it"
            ),
            Severity::Advice,
        ));
    };
    match lock {
        LockState::Missing => None,
        LockState::Unreadable(e) => Some((e.clone(), Severity::Error)),
        LockState::Present(lock) => match (lock.get(icon), lock.verify(icon, &bytes)) {
            (_, Err(e)) => Some((e.to_string(), Severity::Error)),
            (None, Ok(())) => Some((
                format!("`{label}` isn't in {} yet - run `icons fetch` to lock it", guicons_net::LOCK_FILE),
                Severity::Advice,
            )),
            (Some(_), Ok(())) => None,
        },
    }
}

fn build_report(
    file: &Path,
    span: Option<Range<usize>>,
    message: String,
    severity: Severity,
    source_cache: &mut HashMap<PathBuf, String>,
) -> Report {
    let mut diagnostic = MietteDiagnostic::new(message).with_severity(severity);
    if let Some(span) = &span {
        diagnostic = diagnostic.with_label(LabeledSpan::at(span.start..span.end, "here"));
    }

    let mut report = Report::new(diagnostic);
    if span.is_some() {
        let source = source_cache
            .entry(file.to_path_buf())
            .or_insert_with(|| fs::read_to_string(file).unwrap_or_default())
            .clone();
        report = report.with_source_code(NamedSource::new(display_path(file), source));
    }
    report
}

/// Relative to the current directory when possible (both shorter and
/// free of Windows' `\\?\` verbatim-path prefix, which `canonicalize`
/// adds and which `Path::display()` otherwise shows verbatim). Falls back
/// to the absolute path (still with the `\\?\` prefix stripped) when
/// `path` isn't under the current directory at all.
fn display_path(path: &Path) -> String {
    let relative = std::env::current_dir()
        .ok()
        .map(|cwd| fs::canonicalize(&cwd).unwrap_or(cwd))
        .and_then(|cwd| path.strip_prefix(&cwd).ok().map(Path::to_path_buf));
    let rendered = relative.as_deref().unwrap_or(path).display().to_string();
    rendered.strip_prefix(r"\\?\").unwrap_or(&rendered).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn check_reports_zero_errors_for_a_valid_manifest() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("docker.svg"), "<svg/>").unwrap();
        let path = dir.path().join("icons.gui.toml");
        fs::write(&path, "[docker]\nfile = \"docker.svg\"\n").unwrap();

        let (entry_count, reports) = check(&path);
        assert_eq!(entry_count, 1);
        assert!(reports.is_empty());
    }

    #[test]
    fn check_reports_a_pretty_diagnostic_for_an_unknown_field() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("icons.gui.toml");
        fs::write(&path, "[docker]\nfile = \"docker.svg\"\nfile1 = \"docker.svg\"\n").unwrap();

        let (_, reports) = check(&path);
        assert_eq!(reports.len(), 1);
        let rendered = format!("{:?}", reports[0]);
        assert!(rendered.contains("unexpected field"), "{rendered}");
        assert!(rendered.contains("file1"), "{rendered}");
    }

    #[test]
    fn check_reports_an_error_for_a_file_source_pointing_at_a_nonexistent_asset() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("icons.gui.toml");
        // Deliberately never created.
        fs::write(&path, "[docker]\nfile = \"does-not-exist.svg\"\n").unwrap();

        let (entry_count, reports) = check(&path);
        assert_eq!(entry_count, 1);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].severity(), Some(Severity::Error));
        let rendered = format!("{:?}", reports[0]);
        assert!(rendered.contains("does-not-exist.svg"), "{rendered}");
    }

    #[test]
    fn check_reports_advice_for_an_iconify_id_not_yet_cached() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("icons.gui.toml");
        fs::write(&path, "[docker]\niconify = \"mdi:home\"\n").unwrap();

        let (entry_count, reports) = check(&path);
        assert_eq!(entry_count, 1);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].severity(), Some(Severity::Advice));
        let rendered = format!("{:?}", reports[0]);
        assert!(rendered.contains("mdi:home"), "{rendered}");
        assert!(rendered.contains("isn't cached locally"), "{rendered}");
    }

    #[test]
    fn check_reports_nothing_for_an_iconify_id_already_cached() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("icons.gui.toml");
        fs::write(&path, "[docker]\niconify = \"mdi:home\"\n").unwrap();
        let cache_path = dir.path().join(".cache/guicons/mdi/home.svg");
        fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        fs::write(&cache_path, "<svg/>").unwrap();

        let (_, reports) = check(&path);
        assert!(reports.is_empty(), "{reports:?}");
    }

    #[test]
    fn check_reports_an_error_for_a_cached_icon_that_differs_from_the_lock() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("icons.gui.toml");
        fs::write(&path, "[docker]\niconify = \"mdi:home\"\n").unwrap();
        let cache_path = dir.path().join(".cache/guicons/mdi/home.svg");
        fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        fs::write(&cache_path, "<svg>tampered</svg>").unwrap();
        let mut lock = Lock::default();
        lock.set(RemoteIcon::Iconify("mdi:home"), guicons_net::sha256_hex(b"<svg/>"));
        lock.save(&dir.path().join(guicons_net::LOCK_FILE)).unwrap();

        let (_, reports) = check(&path);
        assert_eq!(reports.len(), 1, "{reports:?}");
        assert_eq!(reports[0].severity(), Some(Severity::Error));
        assert!(format!("{:?}", reports[0]).contains("doesn't match"));
    }
}

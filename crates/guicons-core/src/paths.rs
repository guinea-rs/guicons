use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn find_workspace_root(manifest_path: &Path) -> Option<PathBuf> {
    let start = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    find_workspace_root_from(start)
}

pub(crate) fn resolve_workspace_path(workspace_root: &Path, value: &str) -> PathBuf {
    resolve_entry_path(workspace_root, value)
}

pub(crate) fn resolve_entry_path(root: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

/// `dunce::canonicalize`, not `std::fs::canonicalize` - on Windows the
/// latter prefixes the result with the `\\?\` verbatim path marker, so it
/// stops comparing equal to the same path spelled normally.
pub fn canonicalize_or_self(path: &Path) -> PathBuf {
    dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Walks up from `start` looking for the nearest `Cargo.toml` that declares
/// `[workspace]` or `[package]`, i.e. the crate/workspace root.
pub fn find_workspace_root_from(start: &Path) -> Option<PathBuf> {
    let mut current = canonicalize_or_self(start);
    loop {
        let cargo_toml = current.join("Cargo.toml");
        if cargo_toml.exists() {
            let content = fs::read_to_string(&cargo_toml).ok()?;
            if content.contains("[workspace]") || content.contains("[package]") {
                return Some(current);
            }
        }
        current = current.parent()?.to_path_buf();
    }
}

/// A crate's own `icons.gui.toml` can be a *pointer* instead of a real
/// manifest - just a `root_manifest = "<path>"` line, nothing else - for
/// the monorepo case where several crates actually share one manifest
/// that doesn't live next to any of them (e.g. at the repo root). This
/// resolves `path` to whatever it points at (relative to `path`'s own
/// directory), unchanged if `path` isn't a pointer (missing, unreadable,
/// invalid TOML, or just a real manifest with no `root_manifest` key).
/// `crate::load`'s entry points funnel through this, so build.rs's
/// `IconBuild::auto()` and the CLI follow the same pointer transparently -
/// no separate lookup mechanism, no `IconBuild::new(path)` override needed
/// once the pointer file exists.
pub fn resolve_manifest_redirect(path: &Path) -> PathBuf {
    match fs::read_to_string(path) {
        Ok(content) => resolve_manifest_redirect_content(path, &content),
        Err(_) => path.to_path_buf(),
    }
}

/// Like [`resolve_manifest_redirect`], but checking already-in-memory
/// `content` instead of reading `path` from disk.
pub(crate) fn resolve_manifest_redirect_content(path: &Path, content: &str) -> PathBuf {
    let Ok(root) = toml_span::parse(content) else { return path.to_path_buf() };
    let Some(target) = root.pointer("/root_manifest").and_then(|v| v.as_str()) else {
        return path.to_path_buf();
    };
    // Lexically normalized first (unlike the pass-through branches above,
    // which return `path` completely unchanged on purpose - see `load()`'s
    // use of this to decide whether `content_override` still applies): a
    // `root_manifest` value is almost always a `../`-relative path, and
    // `Path::join` never collapses those on its own. This can't lean on
    // `canonicalize_or_self` alone the way the rest of this module does -
    // POSIX `open()` needs every *named* path component to exist to walk
    // through it, `..` included, so a literal, uncollapsed `crates/app/../..`
    // fails to even open when `crates/app` doesn't exist (an unsaved stub
    // file's own directory not existing yet is a real case here, not
    // hypothetical) even though the fully-resolved target does. Windows
    // tolerates this, which is why it only ever showed up on Linux CI.
    // Canonicalized on top of that when possible (same reasoning as
    // elsewhere: two pointers resolving to the same real file should
    // compare equal), falling back to the lexical form otherwise.
    canonicalize_or_self(&normalize_lexically(&path.parent().unwrap_or_else(|| Path::new(".")).join(target)))
}

/// Collapses `.`/`..` components by string manipulation alone, never
/// touching the filesystem - unlike `canonicalize`, doesn't require any
/// path component (including ones a trailing `..` immediately discards)
/// to actually exist. Doesn't resolve symlinks the way a real
/// canonicalize would; only meant as a fallback for a path that can't be
/// canonicalized yet (nothing on disk at that location, possibly not
/// even the intermediate directories) but still needs to be usable as a
/// literal path for `fs::read`/`fs::write` right now.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                if !result.pop() {
                    result.push(component);
                }
            }
            std::path::Component::CurDir => {}
            other => result.push(other),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn resolve_manifest_redirect_follows_a_pointer_to_a_relative_target() {
        let dir = tempdir().unwrap();
        let stub = dir.path().join("crates/app/icons.gui.toml");
        fs::create_dir_all(stub.parent().unwrap()).unwrap();
        fs::write(&stub, "root_manifest = \"../../icons.gui.toml\"\n").unwrap();
        // The target doesn't need to exist for resolution itself, but
        // canonicalize_or_self only cleans up `..` when it does.
        fs::write(dir.path().join("icons.gui.toml"), "").unwrap();

        let resolved = resolve_manifest_redirect(&stub);
        assert_eq!(resolved, canonicalize_or_self(&dir.path().join("icons.gui.toml")));
    }

    #[test]
    fn resolve_manifest_redirect_leaves_a_real_manifest_untouched() {
        let dir = tempdir().unwrap();
        let manifest = dir.path().join("icons.gui.toml");
        fs::write(&manifest, "[docker]\nfile = \"docker.svg\"\n").unwrap();

        assert_eq!(resolve_manifest_redirect(&manifest), manifest);
    }

    #[test]
    fn resolve_manifest_redirect_leaves_a_missing_file_untouched() {
        let missing = Path::new("/does/not/exist/icons.gui.toml");
        assert_eq!(resolve_manifest_redirect(missing), missing);
    }

    /// Nothing here touches disk at all - not the stub, not its parent
    /// directory, not the target - the "brand new, never-saved buffer"
    /// case. `..` still has to collapse lexically for the result to be a
    /// path `fs::read`/`fs::write` can actually open, since POSIX `open()`
    /// requires every named component (including ones a trailing `..`
    /// discards) to exist to walk through it at all.
    #[test]
    fn resolve_manifest_redirect_content_collapses_dotdot_even_when_nothing_exists_on_disk() {
        let stub_path = Path::new("/nonexistent/crates/app/icons.gui.toml");
        let resolved = resolve_manifest_redirect_content(stub_path, "root_manifest = \"../../icons.gui.toml\"\n");
        assert_eq!(resolved, Path::new("/nonexistent/icons.gui.toml"));
    }

    #[test]
    fn resolve_manifest_redirect_content_resolves_a_relative_pointer() {
        let dir = tempdir().unwrap();
        let stub_path = dir.path().join("crates/app/icons.gui.toml");
        // `canonicalize` needs every intermediate directory in the joined
        // `../..`-relative path to actually exist on disk to resolve it at
        // all (Linux is strict about this; Windows' own canonicalize
        // tolerated a non-existent `crates/app/` here, which is exactly
        // why this only failed on the Linux CI runner and not locally).
        fs::create_dir_all(stub_path.parent().unwrap()).unwrap();
        fs::write(dir.path().join("icons.gui.toml"), "").unwrap();

        let resolved = resolve_manifest_redirect_content(&stub_path, "root_manifest = \"../../icons.gui.toml\"\n");
        assert_eq!(resolved, canonicalize_or_self(&dir.path().join("icons.gui.toml")));
    }
}

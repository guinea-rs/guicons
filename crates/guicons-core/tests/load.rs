use guicons_core::{canonicalize_or_self, load_icon_manifest, load_icon_manifest_from_str, IconEntry, IconEntrySource, ManifestError};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, content).unwrap();
    path
}

/// Renders `path` relative to `dir` with `/` separators, for stable snapshots.
/// Canonicalizes with `guicons_core::canonicalize_or_self` (not raw
/// `std::fs::canonicalize`) to match how production actually resolves a
/// manifest's directory (`graph.rs` canonicalizes every manifest path it
/// loads) - using a different canonicalization than production here made
/// `dir`'s resolved form disagree with an entry's already-canonicalized
/// source path on Windows (`\\?\`-prefix and temp-dir aliasing both
/// differ from `dunce`'s output), so the prefix strip below silently fell
/// through to the last-resort "whole absolute path" branch.
fn rel(dir: &Path, path: &Path) -> String {
    let dir_canon = canonicalize_or_self(dir);
    let path_canon = canonicalize_or_self(path);
    if let Ok(suffix) = path_canon.strip_prefix(&dir_canon) {
        return suffix.display().to_string().replace('\\', "/");
    }
    if let Ok(suffix) = path.strip_prefix(dir) {
        return suffix.display().to_string().replace('\\', "/");
    }
    path.display().to_string().replace('\\', "/")
}

#[derive(Debug)]
#[allow(dead_code)] // fields exist only to show up in the debug snapshot
struct EntrySummary {
    key: String,
    family: String,
    variant: Option<String>,
    dynamic: bool,
    source: SourceSummary,
}

#[derive(Debug)]
#[allow(dead_code)] // variants exist only to show up in the debug snapshot
enum SourceSummary {
    File(String),
    Iconify(String),
    Url(String),
    Glyph(String),
}

fn summarize_entries(dir: &Path, entries: &[IconEntry]) -> Vec<EntrySummary> {
    entries
        .iter()
        .map(|entry| EntrySummary {
            key: entry.key().to_string(),
            family: entry.family().to_string(),
            variant: entry.variant().map(str::to_string),
            dynamic: entry.dynamic(),
            source: match entry.source() {
                IconEntrySource::File(path) => SourceSummary::File(rel(dir, path)),
                IconEntrySource::Iconify(id) => SourceSummary::Iconify(id.clone()),
                IconEntrySource::Url(url) => SourceSummary::Url(url.clone()),
                IconEntrySource::Glyph(glyph) => SourceSummary::Glyph(glyph.clone()),
            },
        })
        .collect()
}

#[derive(Debug)]
#[allow(dead_code)] // fields exist only to show up in the debug snapshot
struct ErrorSummary {
    file: String,
    span: Option<(usize, usize)>,
    message: String,
}

fn summarize_errors(dir: &Path, errors: &[ManifestError]) -> Vec<ErrorSummary> {
    errors
        .iter()
        .map(|error| ErrorSummary {
            file: rel(dir, &error.file),
            span: error.span.as_ref().map(|span| (span.start, span.end)),
            message: error.message.clone(),
        })
        .collect()
}

#[test]
fn include_merges_child_manifest_entries() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "icons/nav.gui.toml",
        r#"
        [back]
        file = "back.svg"
        "#,
    );
    let root = write(
        dir.path(),
        "icons.gui.toml",
        r#"
        [link]
        includes = ["icons/nav.gui.toml"]

        [logo]
        file = "logo.svg"
        "#,
    );

    let (manifest, errors) = load_icon_manifest(&root);
    assert!(errors.is_empty(), "{errors:?}");
    insta::assert_debug_snapshot!(summarize_entries(dir.path(), manifest.entries()));
}

#[test]
fn include_merges_child_manifest_providers() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "icons/nav.gui.toml",
        r#"
        [providers.acme-nav]
        variants = ["thin", "light", "bold", "fill", "duotone"]

        [back]
        file = "back.svg"
        "#,
    );
    let root = write(
        dir.path(),
        "icons.gui.toml",
        r#"
        [providers.acme-root]
        variants = ["regular", "filled"]
        sizes = [24]

        [link]
        includes = ["icons/nav.gui.toml"]
        "#,
    );

    let (manifest, errors) = load_icon_manifest(&root);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(manifest.provider("acme-root").is_some());
    assert!(manifest.provider("acme-nav").is_some());
    assert_eq!(manifest.provider("acme-nav").unwrap().variants, vec!["thin", "light", "bold", "fill", "duotone"]);
}

#[test]
fn a_files_own_provider_wins_over_an_included_ones_same_name() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "icons/nav.gui.toml",
        r#"
        [providers.acme]
        variants = ["from-include"]
        "#,
    );
    let root = write(
        dir.path(),
        "icons.gui.toml",
        r#"
        [providers.acme]
        variants = ["from-root"]

        [link]
        includes = ["icons/nav.gui.toml"]
        "#,
    );

    let (manifest, errors) = load_icon_manifest(&root);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(manifest.provider("acme").unwrap().variants, vec!["from-root"]);
}

#[test]
fn cyclic_include_is_reported_and_does_not_hang() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "b.gui.toml",
        r#"
        [link]
        includes = ["a.gui.toml"]
        "#,
    );
    let a = write(
        dir.path(),
        "a.gui.toml",
        r#"
        [link]
        includes = ["b.gui.toml"]
        "#,
    );

    let (_, errors) = load_icon_manifest(&a);
    insta::assert_debug_snapshot!(summarize_errors(dir.path(), &errors));
}

#[test]
fn link_section_must_be_a_table() {
    let dir = tempdir().unwrap();
    let root = write(
        dir.path(),
        "icons.gui.toml",
        r#"
        link = "nope"
        "#,
    );
    let (_, errors) = load_icon_manifest(&root);
    insta::assert_debug_snapshot!(summarize_errors(dir.path(), &errors));
}

#[test]
fn link_includes_must_be_an_array() {
    let dir = tempdir().unwrap();
    let root = write(
        dir.path(),
        "icons.gui.toml",
        r#"
        [link]
        includes = "nope"
        "#,
    );
    let (_, errors) = load_icon_manifest(&root);
    insta::assert_debug_snapshot!(summarize_errors(dir.path(), &errors));
}

#[test]
fn link_includes_entries_must_be_strings() {
    let dir = tempdir().unwrap();
    let root = write(
        dir.path(),
        "icons.gui.toml",
        r#"
        [link]
        includes = [5]
        "#,
    );
    let (_, errors) = load_icon_manifest(&root);
    insta::assert_debug_snapshot!(summarize_errors(dir.path(), &errors));
}

#[test]
fn link_rejects_unknown_fields() {
    let dir = tempdir().unwrap();
    let root = write(
        dir.path(),
        "icons.gui.toml",
        r#"
        [link]
        includes = ["icons/extra.gui.toml"]
        includes1 = ["icons/extra.gui.toml"]
        "#,
    );
    write(dir.path(), "icons/extra.gui.toml", "");
    let (_, errors) = load_icon_manifest(&root);
    insta::assert_debug_snapshot!(summarize_errors(dir.path(), &errors));
}

/// A `[link]`-included file that doesn't exist on disk used to surface as
/// a spanless error attributed to the *missing child's own path*, which
/// showed up in `icons check` with no location at all. It should instead
/// point at the `includes = [...]` entry that named it, in the parent file.
#[test]
fn link_includes_a_nonexistent_file_points_at_the_includes_entry_in_the_parent() {
    let dir = tempdir().unwrap();
    let root = write(
        dir.path(),
        "icons.gui.toml",
        r#"
        [link]
        includes = ["icons/extra.gui.toml"]
        "#,
    );
    // Deliberately never created.
    let (_, errors) = load_icon_manifest(&root);

    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(
        fs::canonicalize(&errors[0].file).unwrap(),
        fs::canonicalize(&root).unwrap(),
        "must be attributed to the parent file, not the missing child"
    );
    assert!(errors[0].span.is_some(), "must point at the `includes = [...]` entry, not be spanless");
    assert!(errors[0].message.contains("icons/extra.gui.toml"), "{}", errors[0].message);
}

#[test]
fn missing_manifest_file_produces_an_error_not_a_panic() {
    let dir = tempdir().unwrap();
    let missing = dir.path().join("does-not-exist.gui.toml");
    let (manifest, errors) = load_icon_manifest(&missing);
    assert!(manifest.entries().is_empty());
    // `std::io::Error`'s message text (embedded in `errors[0].message` via
    // `graph.rs`'s `format!("failed to read file: {e}")`) is OS-specific
    // ("The system cannot find the file specified." on Windows, "No such
    // file or directory" on Linux) - filtered to a placeholder so this
    // snapshot doesn't depend on which CI runner recorded it.
    insta::with_settings!({filters => vec![(r#"failed to read file: [^"]*"#, "failed to read file: <os error>")]}, {
        insta::assert_debug_snapshot!(summarize_errors(dir.path(), &errors));
    });
}

#[test]
fn entries_are_sorted_by_key_across_includes() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "icons/nav.gui.toml",
        r#"
        [middle]
        file = "middle.svg"
        "#,
    );
    let root = write(
        dir.path(),
        "icons.gui.toml",
        r#"
        [link]
        includes = ["icons/nav.gui.toml"]

        [zebra]
        file = "z.svg"

        [alpha]
        file = "a.svg"
        "#,
    );
    let (manifest, errors) = load_icon_manifest(&root);
    assert!(errors.is_empty(), "{errors:?}");
    insta::assert_debug_snapshot!(summarize_entries(dir.path(), manifest.entries()));
}

#[test]
fn load_from_str_prefers_the_given_content_over_the_on_disk_file() {
    let dir = tempdir().unwrap();
    let root = write(
        dir.path(),
        "icons.gui.toml",
        r#"
        [docker]
        file = "docker.svg"
        "#,
    );

    let unsaved_content = r#"
    [docker]
    file = "docker.svg"

    [settings]
    file = "settings.svg"
    "#;
    let (manifest, errors) = load_icon_manifest_from_str(&root, unsaved_content);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(manifest.entry_for_key("settings").is_some(), "should reflect unsaved content, not the file on disk");

    let (on_disk_manifest, errors) = load_icon_manifest(&root);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(on_disk_manifest.entry_for_key("settings").is_none());
}

#[test]
fn load_from_str_still_resolves_includes_from_disk() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "icons/nav.gui.toml",
        r#"
        [back]
        file = "back.svg"
        "#,
    );
    let root_path = dir.path().join("icons.gui.toml");

    let unsaved_content = r#"
    [link]
    includes = ["icons/nav.gui.toml"]

    [docker]
    file = "docker.svg"
    "#;
    let (manifest, errors) = load_icon_manifest_from_str(&root_path, unsaved_content);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(manifest.entry_for_key("docker").is_some());
    assert!(manifest.entry_for_key("back").is_some());
}

#[test]
fn entries_carry_the_file_they_were_declared_in_even_across_includes() {
    let dir = tempdir().unwrap();
    let nav = write(
        dir.path(),
        "icons/nav.gui.toml",
        r#"
        [back]
        file = "back.svg"
        "#,
    );
    let root = write(
        dir.path(),
        "icons.gui.toml",
        r#"
        [link]
        includes = ["icons/nav.gui.toml"]

        [docker]
        file = "docker.svg"
        "#,
    );

    let (manifest, errors) = load_icon_manifest(&root);
    assert!(errors.is_empty(), "{errors:?}");

    let root_canon = canonicalize_or_self(&root);
    let nav_canon = canonicalize_or_self(&nav);

    assert_eq!(manifest.entry_for_key("docker").unwrap().file(), root_canon);
    assert_eq!(manifest.entry_for_key("back").unwrap().file(), nav_canon);
}

/// A crate's own `icons.gui.toml` can be a pointer (`root_manifest = "..."`)
/// to a manifest shared elsewhere, instead of a real manifest - the
/// monorepo case where several crates share one `icons.gui.toml` that
/// doesn't live next to any of them. `load_icon_manifest` should
/// transparently load whatever the pointer resolves to.
#[test]
fn load_icon_manifest_follows_a_root_manifest_pointer() {
    let dir = tempdir().unwrap();
    write(dir.path(), "docker.svg", "<svg/>");
    write(
        dir.path(),
        "icons.gui.toml",
        r#"
        [docker]
        file = "docker.svg"
        "#,
    );
    let stub = write(dir.path(), "crates/app/icons.gui.toml", "root_manifest = \"../../icons.gui.toml\"\n");

    let (manifest, errors) = load_icon_manifest(&stub);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(manifest.entry_for_key("docker").is_some());
    assert_eq!(manifest.manifest_path(), canonicalize_or_self(dir.path()).join("icons.gui.toml"));
}

/// Same as above, but for the editor-tooling path: the pointer file
/// itself is the one with unsaved buffer content (the redirect line),
/// which should still resolve to the real, on-disk manifest rather than
/// being parsed as manifest content in its own right.
#[test]
fn load_icon_manifest_from_str_follows_a_root_manifest_pointer_in_the_edited_buffer() {
    let dir = tempdir().unwrap();
    write(dir.path(), "docker.svg", "<svg/>");
    write(
        dir.path(),
        "icons.gui.toml",
        r#"
        [docker]
        file = "docker.svg"
        "#,
    );
    let stub_path = dir.path().join("crates/app/icons.gui.toml");

    let (manifest, errors) =
        load_icon_manifest_from_str(&stub_path, "root_manifest = \"../../icons.gui.toml\"\n");
    assert!(errors.is_empty(), "{errors:?}");
    assert!(manifest.entry_for_key("docker").is_some());
}

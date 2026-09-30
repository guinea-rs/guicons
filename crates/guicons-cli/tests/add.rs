use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn write(dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, content).unwrap();
    path
}

fn svg(dir: &Path, name: &str) -> String {
    write(dir, name, "<svg/>").to_string_lossy().into_owned()
}

#[test]
fn add_file_source_creates_a_fresh_manifest() {
    let dir = tempdir().unwrap();
    let manifest_path = dir.path().join("icons.gui.toml");
    let logo = svg(dir.path(), "logo.svg");

    let keys = guicons_cli::add(&manifest_path, &logo, Some("uniproc-logo"), &[], None, false).unwrap();
    assert_eq!(keys, vec!["uniproc-logo"]);

    let (manifest, errors) = guicons_core::load_icon_manifest(&manifest_path);
    assert!(errors.is_empty(), "{errors:?}");
    let entry = manifest.entry_for_key("uniproc-logo").unwrap();
    assert_eq!(entry.family(), "uniproc-logo");
}

#[test]
fn add_preserves_existing_content_and_formatting() {
    let dir = tempdir().unwrap();
    let manifest_path = write(
        dir.path(),
        "icons.gui.toml",
        "# a comment worth keeping\n[defaults]\nroot = \"assets\"\n\n[docker]\nfile = \"docker.svg\"\n",
    );

    let logo = svg(dir.path(), "assets/brand/logo.svg");
    guicons_cli::add(&manifest_path, &logo, Some("logo"), &[], None, false).unwrap();

    let content = fs::read_to_string(&manifest_path).unwrap();
    assert!(content.contains("# a comment worth keeping"));
    assert!(content.contains("root = \"assets\""));
    assert!(content.contains("[docker]"));
    assert!(content.contains("file = \"brand/logo.svg\""), "{content}");

    let (manifest, errors) = guicons_core::load_icon_manifest(&manifest_path);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(manifest.entry_for_key("docker").is_some());
    assert!(manifest.entry_for_key("logo").is_some());
}

#[test]
fn add_iconify_without_flags_falls_back_to_flat_entry_without_a_schema() {
    let dir = tempdir().unwrap();
    let manifest_path = dir.path().join("icons.gui.toml");

    let keys = guicons_cli::add(&manifest_path, "fluent:settings-24-regular", None, &[], None, false).unwrap();
    assert_eq!(keys, vec!["settings-24-regular"]);

    let (manifest, errors) = guicons_core::load_icon_manifest(&manifest_path);
    assert!(errors.is_empty(), "{errors:?}");
    let entry = manifest.entry_for_key("settings-24-regular").unwrap();
    assert_eq!(
        entry.source(),
        &guicons_core::IconEntrySource::Iconify("fluent:settings-24-regular".to_string())
    );
}

#[test]
fn add_iconify_without_flags_decomposes_using_provider_schema() {
    let dir = tempdir().unwrap();
    let manifest_path = write(
        dir.path(),
        "icons.gui.toml",
        "[providers.fluent.override]\nvariants = [\"regular\", \"filled\"]\nsizes = [24]\n",
    );

    let keys = guicons_cli::add(&manifest_path, "fluent:settings-24-regular", None, &[], None, false).unwrap();
    assert_eq!(keys, vec!["settings-24-regular"]);

    let (manifest, errors) = guicons_core::load_icon_manifest(&manifest_path);
    assert!(errors.is_empty(), "{errors:?}");
    let entry = manifest.entry_for_key("settings-24-regular").unwrap();
    assert_eq!(entry.family(), "settings");
    assert_eq!(entry.variant(), Some("regular"));
    assert_eq!(entry.size(), Some(24));
}

#[test]
fn add_with_variants_and_size_writes_a_sized_group() {
    let dir = tempdir().unwrap();
    let manifest_path = dir.path().join("icons.gui.toml");

    let mut keys = guicons_cli::add(
        &manifest_path,
        "fluent:settings",
        Some("settings"),
        &["filled".to_string(), "regular".to_string()],
        Some(24),
        false,
    )
    .unwrap();
    keys.sort();
    assert_eq!(keys, vec!["settings-24-filled", "settings-24-regular"]);

    let (manifest, errors) = guicons_core::load_icon_manifest(&manifest_path);
    assert!(errors.is_empty(), "{errors:?}");
    let filled = manifest.entry_for_key("settings-24-filled").unwrap();
    assert_eq!(filled.family(), "settings");
    assert_eq!(filled.size(), Some(24));
    assert_eq!(filled.variant(), Some("filled"));
    assert_eq!(
        filled.source(),
        &guicons_core::IconEntrySource::Iconify("fluent:settings-24-filled".to_string())
    );
}

#[test]
fn add_rejects_duplicate_key_without_force() {
    let dir = tempdir().unwrap();
    let manifest_path = write(dir.path(), "icons.gui.toml", "[docker]\nfile = \"docker.svg\"\n");

    let other = svg(dir.path(), "other.svg");
    let err = guicons_cli::add(&manifest_path, &other, Some("docker"), &[], None, false).unwrap_err();
    assert!(matches!(err, guicons_cli::AddError::AlreadyExists(_)));

    // content must be untouched
    let content = fs::read_to_string(&manifest_path).unwrap();
    assert!(content.contains("docker.svg"));
    assert!(!content.contains("other.svg"));
}

#[test]
fn add_overwrites_duplicate_key_with_force() {
    let dir = tempdir().unwrap();
    let manifest_path = write(dir.path(), "icons.gui.toml", "[docker]\nfile = \"docker.svg\"\n");

    let other = svg(dir.path(), "other.svg");
    guicons_cli::add(&manifest_path, &other, Some("docker"), &[], None, true).unwrap();

    let (manifest, errors) = guicons_core::load_icon_manifest(&manifest_path);
    assert!(errors.is_empty(), "{errors:?}");
    let entry = manifest.entry_for_key("docker").unwrap();
    match entry.source() {
        guicons_core::IconEntrySource::File(path) => {
            assert_eq!(path.file_name().unwrap(), "other.svg");
        }
        other => panic!("expected a file source, got {other:?}"),
    }
}

#[test]
fn add_writes_through_a_pointer_manifest() {
    let dir = tempdir().unwrap();
    let real = write(dir.path(), "icons.gui.toml", "[docker]\niconify = \"mdi:docker\"\n");
    let pointer = write(dir.path(), "crates/app/icons.gui.toml", "root_manifest = \"../../icons.gui.toml\"\n");

    guicons_cli::add(&pointer, "mdi:home", None, &[], None, false).unwrap();

    assert_eq!(fs::read_to_string(&pointer).unwrap(), "root_manifest = \"../../icons.gui.toml\"\n");
    let (manifest, errors) = guicons_core::load_icon_manifest(&real);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(manifest.entry_for_key("home").is_some());
}

#[test]
fn add_keeps_variants_written_as_an_inline_table() {
    let dir = tempdir().unwrap();
    let manifest_path = write(
        dir.path(),
        "icons.gui.toml",
        "[docker]\nvariants = { filled = { iconify = \"mdi:docker\" } }\n",
    );

    guicons_cli::add(&manifest_path, "mdi:docker-outline", Some("docker"), &["regular".to_string()], None, false).unwrap();

    let (manifest, errors) = guicons_core::load_icon_manifest(&manifest_path);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(manifest.entry_for_key("docker-filled").is_some());
    assert!(manifest.entry_for_key("docker-regular").is_some());
}

#[test]
fn add_rejects_a_missing_file() {
    let dir = tempdir().unwrap();
    let manifest_path = dir.path().join("icons.gui.toml");
    let missing = dir.path().join("missing.svg").to_string_lossy().into_owned();

    let err = guicons_cli::add(&manifest_path, &missing, None, &[], None, false).unwrap_err();
    assert!(matches!(err, guicons_cli::AddError::Plan(_)));
    assert!(!manifest_path.exists());
}

#[test]
fn add_variants_without_name_is_an_error() {
    let dir = tempdir().unwrap();
    let manifest_path = dir.path().join("icons.gui.toml");

    let err = guicons_cli::add(
        &manifest_path,
        "fluent:settings",
        None,
        &["filled".to_string()],
        None,
        false,
    )
    .unwrap_err();
    assert!(matches!(err, guicons_cli::AddError::Plan(_)));
}

//! Real end-to-end tests: invokes the actual compiled `icons` binary as a
//! subprocess and asserts on its exit code and stdout/stderr, unlike
//! `add.rs`/`fetch.rs` (which call `guicons_cli::{add, fetch}` directly as
//! library functions - useful for testing that logic, but never exercise
//! argument parsing, exit codes, or the binary's actual printed output).

use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

fn write(dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, content).unwrap();
    path
}

fn icons(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_icons"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run the icons binary")
}

#[test]
fn check_exits_zero_and_prints_ok_for_a_valid_manifest() {
    let dir = tempdir().unwrap();
    write(dir.path(), "docker.svg", "<svg/>");
    write(dir.path(), "icons.gui.toml", "[docker]\nfile = \"docker.svg\"\n");

    let output = icons(dir.path(), &["check"]);

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("OK"), "{stdout}");
    assert!(stdout.contains('1'), "should mention the one icon found: {stdout}");
}

#[test]
fn check_exits_nonzero_and_prints_a_diagnostic_for_an_invalid_manifest() {
    let dir = tempdir().unwrap();
    write(dir.path(), "icons.gui.toml", "[docker]\nfile = \"docker.svg\"\nfile1 = \"docker.svg\"\n");

    let output = icons(dir.path(), &["check"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unexpected field"), "{stderr}");
    assert!(stderr.contains("file1"), "{stderr}");
    assert!(stderr.contains("1 error"), "{stderr}");
}

#[test]
fn check_exits_nonzero_for_a_missing_manifest_file() {
    let dir = tempdir().unwrap();

    let output = icons(dir.path(), &["check", "--manifest", "does-not-exist.gui.toml"]);

    assert!(!output.status.success());
}

/// `icons check` re-reports `guicons_core::load_icon_manifest`'s errors
/// (manifest *shape*: TOML syntax, unknown fields, exactly-one-source) and
/// adds missing-`file` and uncached-iconify checks of its own; it doesn't
/// check the semantic correctness of what fields point to or mean.
/// `glyph`-spec validation and duplicate-`key()` detection
/// used to be gaps of that second kind - now fixed in `guicons-core`
/// (`parse.rs`/`load.rs`) and pinned below as regression tests. Two gaps
/// remain, deliberately not closed (see each test's doc comment for why),
/// pinned as documented current behavior.
mod check_semantic_validation {
    use super::*;

    /// A `file` source pointing at a path that doesn't exist now fails
    /// `check` (fixed in `guicons-cli::check`) - it only used to fail
    /// later, at actual build/materialize time (`guicons-build`/
    /// `guicons-macros`'s `include_bytes!`). `windows-ico` is still not
    /// checked.
    #[test]
    fn check_catches_a_file_source_pointing_at_a_nonexistent_asset() {
        let dir = tempdir().unwrap();
        // Deliberately never created.
        write(dir.path(), "icons.gui.toml", "[docker]\nfile = \"does-not-exist.svg\"\n");

        let output = icons(dir.path(), &["check"]);

        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("does-not-exist.svg"), "{stderr}");
        assert!(stderr.contains("1 error"), "{stderr}");
    }

    /// An `iconify` source that isn't cached locally yet is only advice
    /// (informational) - it can't be confirmed to resolve without a
    /// network fetch, which `check` deliberately never does itself - so
    /// it doesn't fail the command.
    #[test]
    fn check_notes_but_does_not_fail_on_an_uncached_iconify_id() {
        let dir = tempdir().unwrap();
        write(dir.path(), "icons.gui.toml", "[docker]\niconify = \"mdi:home\"\n");

        let output = icons(dir.path(), &["check"]);

        assert!(output.status.success(), "{output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("isn't cached locally"), "{stderr}");
        assert!(stderr.contains("1 note"), "{stderr}");
    }

    /// A hand-written `variants.<name>`/`[family.N]` entry's `variant`/
    /// `size` still isn't checked against `[providers.<name>].variants`/
    /// `.sizes` when the entry uses a plain `file` source (or any source
    /// with no provider association at all) - there's no provider to
    /// check *against* in that case; `[providers.fluent]` happening to be
    /// declared/overridden elsewhere in the same file doesn't mean this
    /// particular `file`-sourced entry has anything to do with `fluent`.
    /// (Contrast with the *auto-built* iconify id case, fixed below -
    /// there the provider association is real and checkable, because
    /// `defaults.provider` says explicitly which schema applies.)
    #[test]
    fn check_does_not_catch_a_variant_name_on_an_unrelated_file_entry() {
        let dir = tempdir().unwrap();
        write(dir.path(), "settings-nonsense.svg", "<svg/>");
        write(
            dir.path(),
            "icons.gui.toml",
            "[providers.fluent.override]\nvariants = [\"regular\", \"filled\"]\n\n\
             [settings]\nvariants.nonsense = { file = \"settings-nonsense.svg\" }\n",
        );

        let output = icons(dir.path(), &["check"]);

        assert!(output.status.success(), "known gap: {output:?}");
    }

    /// A variant/size that doesn't exist in the schema of the provider an
    /// id is *actually being auto-built for* (via `defaults.provider`) now
    /// fails - the constructed id would otherwise 404 silently at fetch
    /// time. Fixed in `parse.rs::parse_entry`.
    #[test]
    fn check_catches_a_variant_not_in_the_auto_build_providers_schema() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "icons.gui.toml",
            "[defaults]\nprovider = \"fluent\"\n\n\
             [providers.fluent.override]\nvariants = [\"regular\", \"filled\"]\n\n\
             [settings]\nvariants.nonsense = {}\n",
        );

        let output = icons(dir.path(), &["check"]);

        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("nonsense"), "{stderr}");
        assert!(stderr.contains("fluent"), "{stderr}");
    }

    /// `glyph = "font:codepoint"` used to be stored as a raw, unvalidated
    /// string at load time - `parse_glyph_spec` (which actually parses and
    /// used to `panic!` on a malformed spec instead of returning a
    /// `Result`) was only ever called later, from `guicons-macros`/
    /// `guicons-build` codegen. Fixed: `parse.rs::parse_entry` now calls
    /// the non-panicking `guicons_core::try_parse_glyph_spec` eagerly and
    /// reports a normal `ManifestError`.
    #[test]
    fn check_catches_a_malformed_glyph_spec() {
        let dir = tempdir().unwrap();
        // Missing the required `font-family:codepoint` colon separator.
        write(dir.path(), "icons.gui.toml", "[settings]\nglyph = \"no-colon-here\"\n");

        let output = icons(dir.path(), &["check"]);

        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("invalid glyph spec"), "{stderr}");
    }

    /// An explicitly-written `iconify = "provider:name"` value's `provider`
    /// prefix is deliberately still NOT checked against builtin/declared
    /// providers - real manifests reference dozens of iconify collections
    /// (`mdi`, `tabler`, `fa`, ...) with no `[providers.*]` schema declared
    /// for most of them at all, since a schema is only needed for
    /// auto-building/decomposing ids, not for using one verbatim (see
    /// `crates/guicons-build/tests/fixtures/basic-file-variants/icons.gui.toml`,
    /// which does exactly this with `mdi:home`). Contrast with the
    /// *auto-built* case below, which is checked, because
    /// `defaults.provider` there is a real, checkable association.
    #[test]
    fn check_does_not_catch_an_unknown_iconify_provider_prefix_on_an_explicit_id() {
        let dir = tempdir().unwrap();
        write(dir.path(), "icons.gui.toml", "[docker]\niconify = \"totally-fake-provider:some-icon\"\n");

        let output = icons(dir.path(), &["check"]);

        assert!(output.status.success(), "known gap: {output:?}");
    }

    /// A typo'd/unknown `defaults.provider` used to auto-build an iconify
    /// id now fails - every id built from it would otherwise silently
    /// reference a provider that will never resolve. Fixed in
    /// `parse.rs::parse_entry`.
    #[test]
    fn check_catches_an_unknown_provider_used_to_auto_build_an_iconify_id() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "icons.gui.toml",
            "[defaults]\nprovider = \"totally-fake-provider\"\n\n[docker]\ndynamic = true\n",
        );

        let output = icons(dir.path(), &["check"]);

        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("totally-fake-provider"), "{stderr}");
    }

    /// Two entries sharing a `key()` across a `[link]` include used to be
    /// silently accepted (entries were just concatenated and sorted) -
    /// `entry_for_key` would then return whichever one `.find()` hit
    /// first, sort-order-dependent. Fixed: `load.rs::check_duplicate_keys`
    /// now runs once over the fully merged entry list.
    #[test]
    fn check_catches_a_duplicate_key_across_a_link_include() {
        let dir = tempdir().unwrap();
        write(dir.path(), "a.svg", "<svg/>");
        write(dir.path(), "b.svg", "<svg/>");
        write(dir.path(), "nav.gui.toml", "[settings]\nfile = \"a.svg\"\n");
        write(
            dir.path(),
            "icons.gui.toml",
            "[link]\nincludes = [\"nav.gui.toml\"]\n\n[settings]\nfile = \"b.svg\"\n",
        );

        let output = icons(dir.path(), &["check"]);

        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("duplicate icon key"), "{stderr}");
        assert!(stderr.contains("settings"), "{stderr}");
    }
}

#[test]
fn add_writes_a_new_entry_and_prints_the_key() {
    let dir = tempdir().unwrap();
    write(dir.path(), "logo.svg", "<svg/>");

    let output = icons(dir.path(), &["add", "logo.svg", "--name", "my-logo"]);

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("my-logo"), "{stdout}");

    let (manifest, errors) = guicons_core::load_icon_manifest(&dir.path().join("icons.gui.toml"));
    assert!(errors.is_empty(), "{errors:?}");
    assert!(manifest.entry_for_key("my-logo").is_some());
}

#[test]
fn add_without_force_fails_on_a_duplicate_key() {
    let dir = tempdir().unwrap();
    write(dir.path(), "logo.svg", "<svg/>");
    write(dir.path(), "icons.gui.toml", "[my-logo]\nfile = \"logo.svg\"\n");

    let output = icons(dir.path(), &["add", "logo.svg", "--name", "my-logo"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--force"), "{stderr}");
}

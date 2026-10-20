use std::path::{Path, PathBuf};
use std::process::Command;

// Spawns a whole separate `cargo test` subprocess against the fixture
// crate with a fresh, uncached target dir - a full from-scratch compile
// every run, several minutes rather than the usual sub-second unit test.
// Excluded from the normal `cargo test --workspace` CI job for exactly
// that reason; run explicitly via `.github/workflows/e2e.yml`.
#[test]
#[ignore = "slow: full nested cargo build from scratch - see module doc comment"]
fn basic_file_variants_fixture_builds() {
    run_fixture("basic-file-variants");
}

fn run_fixture(name: &str) {
    let fixture = fixture_dir(name);
    let target = tempfile::tempdir().unwrap();
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());

    remove_fixture_artifacts(&fixture);

    let output = Command::new(cargo)
        .arg("test")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(fixture.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(target.path())
        .output()
        .unwrap_or_else(|e| panic!("Failed to run fixture `{name}`: {e}"));

    remove_fixture_artifacts(&fixture);

    if !output.status.success() {
        panic!(
            "Fixture `{name}` failed.\n\nstatus:\n{}\n\nstdout:\n{}\n\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn remove_fixture_artifacts(fixture: &Path) {
    let lockfile = fixture.join("Cargo.lock");
    if lockfile.exists() {
        let _ = std::fs::remove_file(&lockfile);
    }

    let target = fixture.join("target");
    if target.exists() {
        let _ = std::fs::remove_dir_all(&target);
    }
}

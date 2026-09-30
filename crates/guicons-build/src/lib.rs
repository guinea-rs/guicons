mod generate;
mod materialize;
mod paths;

use guicons_core::IconManifest;
use materialize::materialize_icons;
use std::path::PathBuf;

pub use guicons_net::ALLOW_NETWORK_ENV;

fn load_icon_manifest(manifest_path: &std::path::Path) -> IconManifest {
    let (manifest, errors) = guicons_core::load_icon_manifest(manifest_path);

    println!("cargo:rerun-if-changed={}", manifest_path.display());
    for source_path in manifest.source_paths() {
        println!("cargo:rerun-if-changed={}", source_path.display());
    }
    println!("cargo:rerun-if-changed={}", guicons_net::lock_path(&manifest).display());
    println!("cargo:rerun-if-env-changed={ALLOW_NETWORK_ENV}");

    if !errors.is_empty() {
        panic!(
            "guicons manifest at {} has errors: {:#?}",
            manifest_path.display(),
            errors
        );
    }

    manifest
}

pub enum Emit {
    Rust,
    Slint,
}

pub struct IconBuild {
    manifest_path: PathBuf,
    out_dir: PathBuf,
    materialized_root: PathBuf,
    emit_rust: bool,
    emit_slint: bool,
    slint_image_resolver: bool,
    windows_reactor_image_resolver: bool,
    egui_image_resolver: bool,
}

impl IconBuild {
    pub fn new(manifest_path: impl Into<PathBuf>) -> Self {
        let out_dir = paths::out_dir();
        Self {
            manifest_path: manifest_path.into(),
            materialized_root: out_dir.clone(),
            out_dir,
            emit_rust: false,
            emit_slint: false,
            slint_image_resolver: false,
            windows_reactor_image_resolver: false,
            egui_image_resolver: false,
        }
    }

    pub fn auto() -> Self {
        Self::new(paths::workspace_manifest_path())
    }

    pub fn materialized_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.materialized_root = root.into();
        self
    }

    pub fn emit(mut self, target: Emit) -> Self {
        match target {
            Emit::Rust => self.emit_rust = true,
            Emit::Slint => self.emit_slint = true,
        }
        self
    }

    pub fn with_slint_image_resolver(mut self) -> Self {
        self.slint_image_resolver = true;
        self
    }

    pub fn with_windows_reactor_image_resolver(mut self) -> Self {
        self.windows_reactor_image_resolver = true;
        self
    }

    pub fn with_egui_image_resolver(mut self) -> Self {
        self.egui_image_resolver = true;
        self
    }

    pub fn build(self) {
        let manifest = load_icon_manifest(&self.manifest_path);
        let icons = materialize_icons(&manifest, &self.materialized_root);

        if self.emit_rust {
            let out_file = self.out_dir.join("icons.rs");
            generate::generate_rust_icon_registry_from_materialized(
                manifest.manifest_path(),
                &out_file,
                &icons,
                self.slint_image_resolver,
                self.windows_reactor_image_resolver,
                self.egui_image_resolver,
            );
        }

        if self.emit_slint {
            let out_file = self.out_dir.join("icons.slint");
            generate::generate_slint_icon_global_from_materialized(&out_file, &icons);
        }
    }
}

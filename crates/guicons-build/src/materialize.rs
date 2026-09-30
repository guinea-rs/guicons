use super::paths::canonicalize_existing;
use guicons_core::{IconEntry, IconEntrySource, IconManifest, ThemePaint};
use guicons_net::{ensure_cached, iconify_cache_path, iconify_url, url_cache_path};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct MaterializedIcon {
    pub(crate) key: String,
    pub(crate) family: String,
    pub(crate) variant: Option<String>,
    pub(crate) size: Option<u16>,
    pub(crate) dynamic: bool,
    pub(crate) backend: MaterializedIconBackend,
}

#[derive(Clone, Debug)]
pub(crate) enum MaterializedIconBackend {
    /// With `paint`, `path` is drawn in the light theme's color.
    Image { path: PathBuf, kind: ImageKind, paint: Option<MaterializedPaint> },
    Glyph { font_family: String, codepoint: char },
}

/// The unpainted SVG, kept so the color can change at runtime, and the
/// dark theme's copy.
#[derive(Clone, Debug)]
pub(crate) struct MaterializedPaint {
    pub(crate) template: PathBuf,
    pub(crate) dark_path: PathBuf,
    pub(crate) colors: ThemePaint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImageKind {
    Svg,
    Png,
}

/// Iconify/URL cache paths are rooted at `manifest.workspace_root()`, not
/// the build script's own `current_dir()` - those two only coincide when
/// `IconBuild::auto()`'s discovery finds `icons.gui.toml` sitting directly
/// alongside the calling crate. `IconBuild::new(path)` lets a manifest live
/// anywhere else (a monorepo root two crates up, say), and `current_dir()`
/// is always the compiling crate's own directory regardless - using it here
/// scattered a separate `.cache/guicons` per crate, invisible to `guicons
/// fetch`, which resolves the cache dir from the manifest's real location.
pub(crate) fn materialize_icons(manifest: &IconManifest, build_out_dir: &Path) -> Vec<MaterializedIcon> {
    let icons_dir = build_out_dir.join("icons");
    let _ = fs::create_dir_all(&icons_dir);

    manifest
        .entries()
        .iter()
        .map(|entry| {
            let backend = match entry.source() {
                IconEntrySource::File(path) => {
                    let output_path =
                        icons_dir.join(format!("{}.{}", output_stem(entry.key()), image_ext(path)));
                    materialize_image(entry, &canonicalize_existing(path), output_path, &icons_dir)
                }
                IconEntrySource::Iconify(id) => {
                    let output_path = icons_dir.join(format!("{}.svg", output_stem(entry.key())));
                    let cached = iconify_cache_path(manifest.workspace_root(), id);
                    ensure_cached(&cached, &iconify_url(id));
                    materialize_image(entry, &cached, output_path, &icons_dir)
                }
                IconEntrySource::Url(url) => {
                    let output_path = icons_dir.join(format!("{}.svg", output_stem(entry.key())));
                    let cached = url_cache_path(manifest.workspace_root(), url);
                    ensure_cached(&cached, url);
                    materialize_image(entry, &cached, output_path, &icons_dir)
                }
                IconEntrySource::Glyph(glyph) => {
                    let (font_family, codepoint) = guicons_core::parse_glyph_spec(glyph, entry.key());
                    MaterializedIconBackend::Glyph {
                        font_family,
                        codepoint,
                    }
                }
            };

            MaterializedIcon {
                key: entry.key().to_string(),
                family: entry.family().to_string(),
                variant: entry.variant().map(str::to_string),
                size: entry.size(),
                dynamic: entry.dynamic(),
                backend,
            }
        })
        .collect()
}

pub(crate) fn output_stem(key: &str) -> String {
    key.replace(['.', '_'], "-")
}

fn materialize_image(entry: &IconEntry, source: &Path, output_path: PathBuf, icons_dir: &Path) -> MaterializedIconBackend {
    let bytes = read(source);
    let kind = image_kind(&output_path);
    let Some(colors) = entry.paint() else {
        write_if_changed(&output_path, &bytes);
        return MaterializedIconBackend::Image { path: output_path, kind, paint: None };
    };
    if !guicons_core::svg_uses_current_color(&bytes) {
        panic!(
            "icon `{}` is declared with `paint`, but {} has no `currentColor` to paint; declare `paint = \"none\"` for it",
            entry.key(),
            source.display()
        );
    }
    let stem = output_stem(entry.key());
    let template = icons_dir.join(format!("{stem}.template.svg"));
    let dark_path = icons_dir.join(format!("{stem}.dark.svg"));
    write_if_changed(&template, &bytes);
    write_if_changed(&output_path, &guicons_core::paint_svg(&bytes, colors.light));
    write_if_changed(&dark_path, &guicons_core::paint_svg(&bytes, colors.dark));
    MaterializedIconBackend::Image {
        path: output_path,
        kind,
        paint: Some(MaterializedPaint { template, dark_path, colors }),
    }
}

fn read(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()))
}

fn write_if_changed(dest: &Path, bytes: &[u8]) {
    let existing = fs::read(dest).unwrap_or_default();
    if existing != bytes {
        if let Some(parent) = dest.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(dest, bytes)
            .unwrap_or_else(|e| panic!("Failed to write {}: {e}", dest.display()));
    }
}

fn image_ext(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()).unwrap_or("svg") {
        "png" => "png",
        _ => "svg",
    }
}

fn image_kind(path: &Path) -> ImageKind {
    match image_ext(path) {
        "png" => ImageKind::Png,
        _ => ImageKind::Svg,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATE: &str = r#"<svg><path fill="currentColor"/></svg>"#;

    fn materialize(manifest: &str, files: &[(&str, &str)]) -> (tempfile::TempDir, Vec<MaterializedIcon>) {
        let dir = tempfile::tempdir().unwrap();
        for (name, content) in files {
            fs::write(dir.path().join(name), content).unwrap();
        }
        let manifest_path = dir.path().join("icons.gui.toml");
        fs::write(&manifest_path, manifest).unwrap();
        let (manifest, errors) = guicons_core::load_icon_manifest(&manifest_path);
        assert!(errors.is_empty(), "{errors:?}");
        let icons = materialize_icons(&manifest, &dir.path().join("out"));
        (dir, icons)
    }

    fn backend<'a>(icons: &'a [MaterializedIcon], key: &str) -> &'a MaterializedIconBackend {
        &icons.iter().find(|icon| icon.key == key).unwrap().backend
    }

    #[test]
    fn painted_icon_keeps_its_template_and_ships_a_copy_per_theme() {
        let (_dir, icons) = materialize(
            "[defaults]\npaint = { light = \"#123456\", dark = \"#abcdef\" }\n\n[gear]\nfile = \"gear.svg\"\n",
            &[("gear.svg", TEMPLATE)],
        );
        let MaterializedIconBackend::Image { path, paint: Some(paint), .. } = backend(&icons, "gear") else {
            panic!("gear should be painted");
        };
        assert_eq!(fs::read_to_string(path).unwrap(), r##"<svg><path fill="#123456"/></svg>"##);
        assert_eq!(fs::read_to_string(&paint.dark_path).unwrap(), r##"<svg><path fill="#abcdef"/></svg>"##);
        assert_eq!(fs::read_to_string(&paint.template).unwrap(), TEMPLATE);
    }

    #[test]
    fn unpainted_icon_is_copied_as_is() {
        let (_dir, icons) = materialize("[gear]\nfile = \"gear.svg\"\n", &[("gear.svg", TEMPLATE)]);
        let MaterializedIconBackend::Image { path, paint: None, .. } = backend(&icons, "gear") else {
            panic!("gear should not be painted");
        };
        assert_eq!(fs::read_to_string(path).unwrap(), TEMPLATE);
    }

    #[test]
    #[should_panic(expected = "has no `currentColor` to paint")]
    fn paint_on_an_svg_without_current_color_fails_the_build() {
        materialize(
            "[defaults]\npaint = \"#123456\"\n\n[logo]\nfile = \"logo.svg\"\n",
            &[("logo.svg", r##"<svg><path fill="#e95420"/></svg>"##)],
        );
    }
}

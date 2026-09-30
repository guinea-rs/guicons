use super::paths::canonicalize_existing;
use guicons_core::{IconEntry, IconEntrySource, IconManifest, ImageFormat, ThemePaint};
use guicons_net::RemoteIcon;
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

/// The unpainted SVG, kept so the color can change at runtime.
#[derive(Clone, Debug)]
pub(crate) struct MaterializedPaint {
    pub(crate) template: PathBuf,
    pub(crate) colors: ThemePaint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImageKind {
    Svg,
    Png,
}

/// Iconify/URL icons come from [`guicons_net::cache_dir`] of the resolved
/// manifest, the same directory `icon!` and `icons fetch` use.
pub(crate) fn materialize_icons(manifest: &IconManifest, build_out_dir: &Path) -> Vec<MaterializedIcon> {
    let icons_dir = build_out_dir.join("icons");
    let _ = fs::create_dir_all(&icons_dir);

    manifest
        .entries()
        .iter()
        .map(|entry| {
            let backend = match entry.source() {
                IconEntrySource::File(path) => materialize_image(entry, &canonicalize_existing(path), &icons_dir),
                IconEntrySource::Iconify(id) => {
                    materialize_image(entry, &cached(manifest, RemoteIcon::Iconify(id)), &icons_dir)
                }
                IconEntrySource::Url(url) => materialize_image(entry, &cached(manifest, RemoteIcon::Url(url)), &icons_dir),
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

fn cached(manifest: &IconManifest, icon: RemoteIcon<'_>) -> PathBuf {
    guicons_net::ensure_cached(manifest, icon).unwrap_or_else(|e| panic!("{e}"))
}

pub(crate) fn output_stem(key: &str) -> String {
    key.replace(['.', '_'], "-")
}

fn materialize_image(entry: &IconEntry, source: &Path, icons_dir: &Path) -> MaterializedIconBackend {
    let bytes = read(source);
    let (kind, extension) = match ImageFormat::sniff(&bytes) {
        Some(ImageFormat::Svg) => (ImageKind::Svg, "svg"),
        Some(ImageFormat::Png) => (ImageKind::Png, "png"),
        None => panic!("icon `{}`: {} is neither SVG nor PNG", entry.key(), source.display()),
    };
    let output_path = icons_dir.join(format!("{}.{extension}", output_stem(entry.key())));
    let colors = entry.paint().filter(|_| kind == ImageKind::Svg);
    let Some(colors) = colors else {
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
    write_if_changed(&template, &bytes);
    write_if_changed(&output_path, &guicons_core::paint_svg(&bytes, colors.light));
    MaterializedIconBackend::Image {
        path: output_path,
        kind,
        paint: Some(MaterializedPaint { template, colors }),
    }
}

fn read(path: &Path) -> Vec<u8> {
    println!("cargo:rerun-if-changed={}", path.display());
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
    fn painted_icon_keeps_its_template_and_a_light_copy() {
        let (_dir, icons) = materialize(
            "[defaults]\npaint = { light = \"#123456\", dark = \"#abcdef\" }\n\n[gear]\nfile = \"gear.svg\"\n",
            &[("gear.svg", TEMPLATE)],
        );
        let MaterializedIconBackend::Image { path, paint: Some(paint), .. } = backend(&icons, "gear") else {
            panic!("gear should be painted");
        };
        assert_eq!(fs::read_to_string(path).unwrap(), r##"<svg><path fill="#123456"/></svg>"##);
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
    fn format_comes_from_content_not_extension() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Logo.PNG"), b"\x89PNG\r\n\x1a\nrest").unwrap();
        fs::write(dir.path().join("gear.svg"), TEMPLATE).unwrap();
        let manifest_path = dir.path().join("icons.gui.toml");
        fs::write(
            &manifest_path,
            "[defaults]\npaint = \"#123456\"\n\n[logo]\nfile = \"Logo.PNG\"\n\n[gear]\nfile = \"gear.svg\"\n",
        )
        .unwrap();
        let (manifest, errors) = guicons_core::load_icon_manifest(&manifest_path);
        assert!(errors.is_empty(), "{errors:?}");
        let icons = materialize_icons(&manifest, &dir.path().join("out"));

        let MaterializedIconBackend::Image { path, kind, paint } = backend(&icons, "logo") else {
            panic!("logo should be an image");
        };
        assert_eq!(*kind, ImageKind::Png);
        assert!(paint.is_none());
        assert_eq!(path.extension().unwrap(), "png");
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

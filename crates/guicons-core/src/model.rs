use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct IconManifest {
    pub(crate) manifest_path: PathBuf,
    pub(crate) workspace_root: PathBuf,
    pub(crate) source_paths: Vec<PathBuf>,
    pub(crate) entries: Vec<IconEntry>,
    pub(crate) providers: HashMap<String, ProviderSchema>,
    pub(crate) default_paint: Option<Paint>,
    pub(crate) roots: Vec<PathBuf>,
}

/// The known variants/sizes for one icon provider, from `[providers.<name>]`.
/// Lets `decompose_iconify_id` tell which trailing `-segment`s of a pasted
/// iconify id are suffixes versus part of the icon's own name.
#[derive(Clone, Debug, Default)]
pub struct ProviderSchema {
    pub variants: Vec<String>,
    pub sizes: Vec<u16>,
    pub paint: Option<Paint>,
}

/// A declared `paint` value: `"none"`, one `#rgb`/`#rrggbb` color for every
/// theme, or `{ light = ..., dark = ... }`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Paint {
    None,
    Colors(ThemePaint),
}

/// The colors an icon's `currentColor` is painted with, per theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ThemePaint {
    pub light: PaintColor,
    pub dark: PaintColor,
}

/// The color an icon's `currentColor` is painted with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PaintColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Paint {
    /// The string form: `"none"` or one color for every theme.
    pub fn parse(value: &str) -> Result<Self, String> {
        if value == "none" {
            return Ok(Self::None);
        }
        PaintColor::parse(value)
            .map(|color| Self::Colors(ThemePaint::uniform(color)))
            .map_err(|_| {
                format!("`paint` must be `\"none\"`, a `#rgb`/`#rrggbb` color, or `{{ light = ..., dark = ... }}`, got `{value}`")
            })
    }

    pub fn colors(self) -> Option<ThemePaint> {
        match self {
            Self::None => None,
            Self::Colors(colors) => Some(colors),
        }
    }
}

impl ThemePaint {
    pub fn uniform(color: PaintColor) -> Self {
        Self { light: color, dark: color }
    }
}

impl PaintColor {
    pub fn parse(value: &str) -> Result<Self, String> {
        let invalid = || format!("`paint` colors must be `#rgb`/`#rrggbb`, got `{value}`");
        let hex = value.strip_prefix('#').ok_or_else(invalid)?;
        if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid());
        }
        let channel = |digits: &str| u8::from_str_radix(digits, 16).map_err(|_| invalid());
        match hex.len() {
            3 => {
                let expand = |index: usize| channel(&hex[index..=index].repeat(2));
                Ok(Self { r: expand(0)?, g: expand(1)?, b: expand(2)? })
            }
            6 => Ok(Self { r: channel(&hex[0..2])?, g: channel(&hex[2..4])?, b: channel(&hex[4..6])? }),
            _ => Err(invalid()),
        }
    }

    /// `#rrggbb`, lowercase.
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Svg,
    Png,
}

impl ImageFormat {
    /// By content: the PNG signature, or text that contains an `<svg>`
    /// element and isn't an HTML page.
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Some(Self::Png);
        }
        let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
        let text = text.trim_start_matches('\u{feff}').trim_start();
        (text.starts_with('<') && text.contains("<svg") && !text.contains("<html")).then_some(Self::Svg)
    }

    /// By extension, ignoring case.
    pub fn from_extension(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "svg" => Some(Self::Svg),
            "png" => Some(Self::Png),
            _ => None,
        }
    }

    /// By content when the file can be read, otherwise by extension.
    pub fn of_file(path: &Path) -> Option<Self> {
        match std::fs::read(path) {
            Ok(bytes) => Self::sniff(&bytes),
            Err(_) => Self::from_extension(path),
        }
    }
}

/// Whether an SVG has anything for `paint` to recolor.
pub fn svg_uses_current_color(svg: &[u8]) -> bool {
    svg.windows(CURRENT_COLOR.len())
        .any(|window| window.eq_ignore_ascii_case(CURRENT_COLOR.as_bytes()))
}

/// `svg` with every `currentColor` replaced by `color`.
pub fn paint_svg(svg: &[u8], color: PaintColor) -> Vec<u8> {
    let hex = color.to_hex();
    let needle = CURRENT_COLOR.as_bytes();
    let mut out = Vec::with_capacity(svg.len());
    let mut rest = svg;
    while !rest.is_empty() {
        if rest.len() >= needle.len() && rest[..needle.len()].eq_ignore_ascii_case(needle) {
            out.extend_from_slice(hex.as_bytes());
            rest = &rest[needle.len()..];
        } else {
            out.push(rest[0]);
            rest = &rest[1..];
        }
    }
    out
}

const CURRENT_COLOR: &str = "currentColor";

#[derive(Clone, Debug)]
pub struct IconEntry {
    pub(crate) key: String,
    pub(crate) family: String,
    pub(crate) variant: Option<String>,
    pub(crate) size: Option<u16>,
    pub(crate) source: IconEntrySource,
    pub(crate) dynamic: bool,
    pub(crate) windows_ico: Option<PathBuf>,
    pub(crate) paint: Option<ThemePaint>,
    pub(crate) span: Range<usize>,
    pub(crate) file: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IconEntrySource {
    File(PathBuf),
    Iconify(String),
    Url(String),
    Glyph(String),
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ManifestDefaults {
    pub(crate) roots: Vec<PathBuf>,
    /// The roots a `[defaults]` declared, here or in an including file.
    pub(crate) declared_roots: Vec<PathBuf>,
    pub(crate) provider: Option<String>,
    pub(crate) size: Option<u16>,
    pub(crate) paint: Option<Paint>,
}

impl IconManifest {
    pub fn entries(&self) -> &[IconEntry] {
        &self.entries
    }

    pub fn manifest_path(&self) -> &Path {
        &self.manifest_path
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn source_paths(&self) -> &[PathBuf] {
        &self.source_paths
    }

    /// Where the root manifest's `file` sources are looked up, in order.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Renders `path` for display: relative to the workspace root or the
    /// manifest's own directory when possible (both are "not noise"),
    /// falling back to the absolute path only if neither contains it.
    /// Always forward-slashed - `Path::display()` on Windows keeps `\`,
    /// and `{:?}`/Debug escapes it as `\\`, both of which are just noise
    /// here (also strips Windows' `\\?\` verbatim-path prefix, which
    /// `canonicalize`d paths - what every path here is - otherwise carry).
    pub fn display_path(&self, path: &Path) -> String {
        if let Ok(rel) = path.strip_prefix(self.workspace_root()) {
            return normalize_slashes(rel);
        }
        if let Some(manifest_dir) = self.manifest_path().parent() {
            if let Ok(rel) = path.strip_prefix(manifest_dir) {
                return normalize_slashes(rel);
            }
        }
        normalize_slashes(path)
    }

    pub fn entry_for_key(&self, key: &str) -> Option<&IconEntry> {
        self.entries.iter().find(|entry| entry.key == key)
    }

    pub fn entry_for_family_variant(
        &self,
        family: &str,
        size: Option<u16>,
        variant: Option<&str>,
    ) -> Option<&IconEntry> {
        self.entries.iter().find(|entry| {
            entry.family == family && entry.size == size && entry.variant.as_deref() == variant
        })
    }

    pub fn provider(&self, name: &str) -> Option<&ProviderSchema> {
        self.providers.get(name)
    }

    pub fn provider_names(&self) -> impl Iterator<Item = &str> {
        self.providers.keys().map(String::as_str)
    }

    /// Paint for an iconify id used outside any entry (`icon!("mdi:home")`):
    /// the provider's `paint`, then the root manifest's `[defaults]`.
    pub fn paint_for_iconify(&self, id: &str) -> Option<ThemePaint> {
        let provider = id.split_once(':').map(|(provider, _)| provider);
        provider
            .and_then(|provider| self.providers.get(provider))
            .and_then(|schema| schema.paint)
            .or(self.default_paint)
            .and_then(Paint::colors)
    }
}

impl IconEntry {
    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn family(&self) -> &str {
        &self.family
    }

    pub fn variant(&self) -> Option<&str> {
        self.variant.as_deref()
    }

    /// From a `[family.N]` numeric path segment, or inherited from
    /// `defaults.size` - not a separate manifest keyword.
    pub fn size(&self) -> Option<u16> {
        self.size
    }

    pub fn source(&self) -> &IconEntrySource {
        &self.source
    }

    pub fn dynamic(&self) -> bool {
        self.dynamic
    }

    /// Per-theme colors for the icon's `currentColor`, resolved from the entry,
    /// its enclosing tables, its iconify provider and `[defaults]`, nearest first.
    pub fn paint(&self) -> Option<ThemePaint> {
        self.paint
    }

    // TODO: app-icon bundling (.ico/.icns generation, of which this is the
    // last remaining piece) was dropped from guicons's own scope - it's a
    // packaging/distribution concern, not "icons looked up at runtime by
    // an app's own UI". This field/accessor doesn't belong on the main
    // IconEntry model; move it out to a separate crate (or drop it) rather
    // than growing more usages of it here.
    pub fn windows_ico(&self) -> Option<&Path> {
        self.windows_ico.as_deref()
    }

    /// Byte range of this entry's table in the manifest source (the
    /// variant's inline table, or the flat entry's table) - for editor
    /// tooling that needs to map a cursor position back to an entry.
    /// Only meaningful together with [`Self::file`] - spans from
    /// different files (e.g. across `[link]`) can overlap numerically.
    pub fn span(&self) -> Range<usize> {
        self.span.clone()
    }

    /// The specific manifest file this entry was declared in - the root
    /// manifest, or one of its `[link]`d files.
    pub fn file(&self) -> &Path {
        &self.file
    }
}

fn normalize_slashes(path: &Path) -> String {
    let rendered = path.display().to_string().replace('\\', "/");
    rendered.strip_prefix(r"//?/").unwrap_or(&rendered).to_string()
}

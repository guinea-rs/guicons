use crate::{Color, IconData};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex, PoisonError};
use windows_reactor::{EncodedImage, FontIcon, Image, ImageIcon, LayoutControl, View};

pub const DEFAULT_ICON_SIZE: f64 = 16.0;

static SVG_FILES: LazyLock<Mutex<HashMap<usize, String>>> = LazyLock::new(Default::default);

impl From<windows_reactor::Color> for Color {
    fn from(color: windows_reactor::Color) -> Self {
        Self { r: color.r, g: color.g, b: color.b }
    }
}

enum Source {
    Path(String),
    Encoded(&'static [u8]),
    None,
}

impl Source {
    /// WinUI decodes in-memory bytes only as a bitmap, so an SVG is written
    /// once per distinct content under the temp directory and loaded from there.
    fn from_data(data: IconData) -> Self {
        match data {
            IconData::Png(bytes) => Self::Encoded(bytes),
            IconData::Svg(_) | IconData::PaintedSvg { .. } => {
                data.svg_bytes().and_then(svg_file).map(Self::Path).unwrap_or(Self::None)
            }
            IconData::Glyph { .. } => Self::None,
        }
    }

    fn image(self) -> Image {
        let image = Image::new();
        match self {
            Self::Path(path) if path.contains("://") => image.source(path).unwrap_or_default(),
            Self::Path(path) => image.source_file(path).unwrap_or_default(),
            Self::Encoded(bytes) => image.source_data(EncodedImage::from_static(bytes)),
            Self::None => Image::default(),
        }
    }

    fn image_icon(self) -> ImageIcon {
        let icon = ImageIcon::new();
        match self {
            Self::Path(path) if path.contains("://") => icon.source(path).unwrap_or_default(),
            Self::Path(path) => icon.source_file(path).unwrap_or_default(),
            Self::Encoded(bytes) => icon.source_data(EncodedImage::from_static(bytes)),
            Self::None => ImageIcon::default(),
        }
    }
}

fn svg_file(svg: &'static [u8]) -> Option<String> {
    let mut files = SVG_FILES.lock().unwrap_or_else(PoisonError::into_inner);
    let path = files
        .entry(svg.as_ptr() as usize)
        .or_insert_with(|| write_svg_file(svg).map(|path| path.to_string_lossy().into_owned()).unwrap_or_default());
    (!path.is_empty()).then(|| path.clone())
}

fn write_svg_file(svg: &[u8]) -> std::io::Result<PathBuf> {
    let mut hasher = DefaultHasher::new();
    svg.hash(&mut hasher);
    let dir = std::env::temp_dir().join("guicons");
    let path = dir.join(format!("{:016x}.svg", hasher.finish()));
    if std::fs::read(&path).is_ok_and(|existing| existing == svg) {
        return Ok(path);
    }
    std::fs::create_dir_all(&dir)?;
    let partial = dir.join(format!("{:016x}.{}.tmp", hasher.finish(), std::process::id()));
    std::fs::write(&partial, svg)?;
    std::fs::rename(&partial, &path)?;
    Ok(path)
}

/// `Image` for icon data embedded in the binary; an unset `Image` for a glyph.
pub fn image_from_data(data: IconData) -> Image {
    Source::from_data(data).image()
}

/// `ImageIcon` for icon data embedded in the binary; an unset `ImageIcon` for a glyph.
pub fn image_icon_from_data(data: IconData) -> ImageIcon {
    Source::from_data(data).image_icon()
}

/// `Image` for a file path or URI; an unset `Image` if the source is rejected.
pub fn image_from_path(path: &str) -> Image {
    Source::Path(path.to_string()).image()
}

/// `ImageIcon` for a file path or URI; an unset `ImageIcon` if the source is rejected.
pub fn image_icon_from_path(path: &str) -> ImageIcon {
    Source::Path(path.to_string()).image_icon()
}

/// What `icon!(...)` expands to under the `windows-reactor` feature.
pub struct IconBuilder {
    source: Source,
    width: Option<f64>,
    height: Option<f64>,
}

/// An icon loaded from a file path or URI at runtime.
pub fn icon_builder(path: impl Into<String>) -> IconBuilder {
    IconBuilder { source: Source::Path(path.into()), width: None, height: None }
}

/// An icon from data embedded in the binary.
pub fn data_icon_builder(data: IconData) -> IconBuilder {
    IconBuilder { source: Source::from_data(data), width: None, height: None }
}

impl IconBuilder {
    pub fn size(mut self, size: f64) -> Self {
        self.width = Some(size);
        self.height = Some(size);
        self
    }

    pub fn width(mut self, width: f64) -> Self {
        self.width = Some(width);
        self
    }

    pub fn height(mut self, height: f64) -> Self {
        self.height = Some(height);
        self
    }

    /// An `ImageIcon` for an icon slot (`.icon(...)`), sized only if a size was set.
    pub fn build(self) -> View {
        self.source.image_icon().width(self.width).height(self.height).into()
    }

    /// A standalone `Image`, [`DEFAULT_ICON_SIZE`] unless a size was set.
    pub fn build_element(self) -> View {
        self.source
            .image()
            .width(self.width.unwrap_or(DEFAULT_ICON_SIZE))
            .height(self.height.unwrap_or(DEFAULT_ICON_SIZE))
            .into()
    }
}

impl From<IconBuilder> for View {
    fn from(builder: IconBuilder) -> Self {
        builder.build()
    }
}

/// A `FontIcon` for `codepoint`, rendered in the platform symbol font.
pub fn glyph_icon(codepoint: char) -> View {
    FontIcon::new().glyph(codepoint.to_string()).into()
}

use crate::{Color, IconData};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex, PoisonError};
use windows_reactor::{EncodedImage, FontIcon, Icon, Image, ImageIcon, View};

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
    Glyph(char),
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
            Self::Glyph(_) | Self::None => Image::default(),
        }
    }

    fn image_icon(self) -> ImageIcon {
        let icon = ImageIcon::new();
        match self {
            Self::Path(path) if path.contains("://") => icon.source(path).unwrap_or_default(),
            Self::Path(path) => icon.source_file(path).unwrap_or_default(),
            Self::Encoded(bytes) => icon.source_data(EncodedImage::from_static(bytes)),
            Self::Glyph(_) | Self::None => ImageIcon::default(),
        }
    }

    fn icon(self) -> Icon {
        let icon = match self {
            Self::Path(path) if path.contains("://") => Icon::image_uri(path).ok(),
            Self::Path(path) => Icon::image_file(path).ok(),
            Self::Encoded(bytes) => Some(Icon::image_data(EncodedImage::from_static(bytes))),
            Self::Glyph(codepoint) => Some(Icon::font(codepoint.to_string())),
            Self::None => None,
        };
        icon.unwrap_or_else(|| Icon::font(""))
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

    /// An `Icon` for an icon slot (`.icon(...)`); the slot sizes it.
    pub fn build(self) -> Icon {
        self.source.icon()
    }

    /// A standalone element, [`DEFAULT_ICON_SIZE`] unless a size was set.
    pub fn build_element(self) -> View {
        let width = self.width.unwrap_or(DEFAULT_ICON_SIZE);
        let height = self.height.unwrap_or(DEFAULT_ICON_SIZE);
        match self.source {
            Source::Glyph(codepoint) => FontIcon::new().glyph(codepoint.to_string()).width(width).height(height).into(),
            source => source.image().width(width).height(height).into(),
        }
    }
}

impl From<IconBuilder> for View {
    fn from(builder: IconBuilder) -> Self {
        builder.build_element()
    }
}

impl From<IconBuilder> for Icon {
    fn from(builder: IconBuilder) -> Self {
        builder.build()
    }
}

/// A `FontIcon` for `codepoint`, rendered in the platform symbol font.
pub fn glyph_icon(codepoint: char) -> IconBuilder {
    IconBuilder { source: Source::Glyph(codepoint), width: None, height: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

    #[test]
    fn embedded_data_builds_an_image_icon() {
        let expected = Icon::image_data(EncodedImage::from_static(PNG));
        assert_eq!(data_icon_builder(IconData::Png(PNG)).size(24.0).build(), expected);
    }

    #[test]
    fn file_path_builds_an_image_file_icon() {
        let expected = Icon::image_file("C:/icons/gear.svg").unwrap();
        assert_eq!(icon_builder("C:/icons/gear.svg").build(), expected);
    }

    #[test]
    fn uri_builds_an_image_uri_icon() {
        let expected = Icon::image_uri("ms-appx:///Assets/gear.png").unwrap();
        assert_eq!(icon_builder("ms-appx:///Assets/gear.png").build(), expected);
    }

    #[test]
    fn glyph_builds_a_font_icon() {
        assert_eq!(glyph_icon('\u{E700}').size(20.0).build(), Icon::font("\u{E700}"));
    }

    #[test]
    fn glyph_element_is_a_sized_font_icon() {
        let expected: View = FontIcon::new().glyph("\u{E700}").width(DEFAULT_ICON_SIZE).height(DEFAULT_ICON_SIZE).into();
        assert_eq!(glyph_icon('\u{E700}').build_element(), expected);
    }

    #[test]
    fn into_view_is_the_standalone_element() {
        let view: View = data_icon_builder(IconData::Png(PNG)).into();
        assert_eq!(view, data_icon_builder(IconData::Png(PNG)).build_element());
    }
}

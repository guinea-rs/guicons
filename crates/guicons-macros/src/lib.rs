use guicons_core::selector::{classify_segments, IconSelector, PathSegment};
use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};
use quote::quote;
use std::path::PathBuf;
use syn::parse::{Parse, ParseStream};
use syn::{parse_macro_input, Error, Expr, LitInt, LitStr, Result, Token};

#[cfg(any(
    all(feature = "slint", feature = "iced"),
    all(feature = "slint", feature = "windows-reactor"),
    all(feature = "slint", feature = "egui"),
    all(feature = "iced", feature = "windows-reactor"),
    all(feature = "iced", feature = "egui"),
    all(feature = "windows-reactor", feature = "egui")
))]
compile_error!(
    "guicons-macros: enable only one of the `slint`/`iced`/`windows-reactor`/`egui` features at a time - `icon!` picks its \
     target automatically from whichever is active. Use `icon_data!` if you need the plain `IconData` \
     regardless of which GUI feature is enabled."
);

/// Which native type `icon!` emits, chosen automatically from whichever of
/// `guicons-macros`' GUI features is active (mirrored from `guicons`' own
/// features of the same name). Falls back to `IconData` when none is enabled.
#[derive(Clone, Copy)]
enum Target {
    Data,
    #[cfg(feature = "slint")]
    Slint,
    #[cfg(feature = "iced")]
    Iced,
    #[cfg(feature = "windows-reactor")]
    WindowsReactor,
    #[cfg(feature = "egui")]
    Egui,
}

fn active_target() -> Target {
    #[cfg(feature = "slint")]
    return Target::Slint;
    #[cfg(feature = "iced")]
    return Target::Iced;
    #[cfg(feature = "windows-reactor")]
    return Target::WindowsReactor;
    #[cfg(feature = "egui")]
    return Target::Egui;
    #[allow(unreachable_code)]
    Target::Data
}

/// Resolves a selector straight to the native icon type for whichever GUI
/// feature is active (`slint`/`iced`/`windows-reactor`/`egui`), or to `IconData` if none is - no
/// manifest-key indirection, no wrapping call needed at the use site. Use
/// [`icon_data!`] to always get `IconData`, regardless of active features.
#[proc_macro]
pub fn icon(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as IconMacroInput);
    expand_icon(input, active_target())
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// Like [`icon!`], but always resolves to plain `IconData` - the explicit
/// escape hatch from `icon!`'s automatic native-type target.
#[proc_macro]
pub fn icon_data(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as IconMacroInput);
    expand_icon(input, Target::Data)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// Resolves a `family`/`family.variant`/`family.size`/`family.size.variant`
/// selector to the manifest's `IconKey` constant, for callers that need
/// runtime-swappable resolution (theming, hot-reload via `IconResolver`).
#[proc_macro]
pub fn icon_key(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as IconMacroInput);
    expand_icon_key(input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

struct IconMacroInput {
    selector: IconSelector,
    module: Ident,
    color: Option<Expr>,
}

impl Parse for IconMacroInput {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let selector = if input.peek(LitStr) {
            let literal: LitStr = input.parse()?;
            parse_selector_literal(&literal)?
        } else {
            parse_selector_path(input)?
        };

        let mut module = Ident::new("icons", Span::call_site());
        let mut color = None;
        while input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                break;
            }
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            if key == "module" {
                module = input.parse()?;
            } else if key == "color" {
                color = Some(input.parse()?);
            } else {
                return Err(Error::new_spanned(key, "expected `module = ...` or `color = ...`"));
            }
        }

        if !input.is_empty() {
            return Err(input.error("unexpected tokens in guicons::icon! input"));
        }

        Ok(Self { selector, module, color })
    }
}

fn expand_icon(input: IconMacroInput, target: Target) -> Result<proc_macro2::TokenStream> {
    let color = input.color.as_ref();
    match input.selector {
        IconSelector::FamilyVariant { family, size, variant } => {
            expand_family_variant_data(&family, size, variant.as_deref(), color, target)
        }
        IconSelector::Iconify(id) => expand_iconify_literal(&id, color, target),
    }
}

fn expand_icon_key(input: IconMacroInput) -> Result<proc_macro2::TokenStream> {
    if let Some(color) = &input.color {
        return Err(Error::new_spanned(color, "`icon_key!` names an icon, it has no color"));
    }
    match input.selector {
        IconSelector::FamilyVariant { family, size, variant } => {
            expand_family_variant_key(&family, size, variant.as_deref(), input.module)
        }
        IconSelector::Iconify(id) => Err(Error::new(
            Span::call_site(),
            format!(
                "`icon_key!` doesn't support iconify literals - there's no manifest key for a raw iconify id `{id}`, use `icon!` instead"
            ),
        )),
    }
}

fn expand_family_variant_key(
    family: &str,
    size: Option<u16>,
    variant: Option<&str>,
    module: Ident,
) -> Result<proc_macro2::TokenStream> {
    let manifest_path = manifest_dir()?.join("icons.gui.toml");
    let manifest = load_manifest(&manifest_path)?;
    let key = match manifest.entry_for_family_variant(family, size, variant) {
        Some(entry) => entry.key().to_string(),
        None => {
            return Err(Error::new(
                Span::call_site(),
                unknown_icon_message(&manifest, family, size, variant),
            ));
        }
    };

    let key_ident = Ident::new(&guicons_core::rust_const_name(&key), Span::call_site());
    Ok(quote! { #module::keys::#key_ident })
}

/// What an entry's source resolves to, before deciding which native type
/// (or plain `IconData`) to wrap it in for the requested [`Target`].
enum ResolvedSource {
    Image { path: String, kind: &'static str },
    Painted { path: String, colors: proc_macro2::TokenStream, color: proc_macro2::TokenStream },
    Glyph { font_family: String, codepoint: char },
}

/// Turns an SVG declared with `paint` into [`ResolvedSource::Painted`], in the
/// declared per-theme colors or the caller's `color = ...`.
fn apply_paint(
    resolved: ResolvedSource,
    paint: Option<guicons_core::ThemePaint>,
    color: Option<&Expr>,
    name: &str,
) -> Result<ResolvedSource> {
    let Some(paint) = paint else {
        if let Some(color) = color {
            return Err(Error::new_spanned(
                color,
                format!("icon `{name}` isn't declared with `paint`, there is nothing to recolor"),
            ));
        }
        return Ok(resolved);
    };
    let ResolvedSource::Image { path, .. } = resolved else {
        return Ok(resolved);
    };
    if let Ok(svg) = std::fs::read(&path) {
        if !guicons_core::svg_uses_current_color(&svg) {
            return Err(Error::new(
                Span::call_site(),
                format!(
                    "icon `{name}` is declared with `paint`, but {path} has no `currentColor` to paint; declare `paint = \"none\"` for it"
                ),
            ));
        }
    }
    let rgb = |guicons_core::PaintColor { r, g, b }: guicons_core::PaintColor| quote! { guicons::Color::rgb(#r, #g, #b) };
    let (light, dark) = (rgb(paint.light), rgb(paint.dark));
    let colors = quote! { guicons::ThemeColors { light: #light, dark: #dark } };
    let color = match color {
        Some(color) => quote! { ::core::option::Option::Some(guicons::Color::from(#color)) },
        None => quote! { ::core::option::Option::None },
    };
    Ok(ResolvedSource::Painted { path, colors, color })
}

fn expand_family_variant_data(
    family: &str,
    size: Option<u16>,
    variant: Option<&str>,
    color: Option<&Expr>,
    target: Target,
) -> Result<proc_macro2::TokenStream> {
    let manifest_path = manifest_dir()?.join("icons.gui.toml");
    let manifest = load_manifest(&manifest_path)?;
    let entry = match manifest.entry_for_family_variant(family, size, variant) {
        Some(entry) => entry,
        None => {
            return Err(Error::new(
                Span::call_site(),
                unknown_icon_message(&manifest, family, size, variant),
            ));
        }
    };

    let resolved = match entry.source() {
        guicons_core::IconEntrySource::File(path) => ResolvedSource::Image {
            kind: image_kind(path),
            path: path.to_string_lossy().into_owned(),
        },
        guicons_core::IconEntrySource::Iconify(id) => resolve_iconify_source(id)?,
        guicons_core::IconEntrySource::Url(url) => resolve_url_source(url)?,
        guicons_core::IconEntrySource::Glyph(spec) => {
            let (font_family, codepoint) = guicons_core::parse_glyph_spec(spec, entry.key());
            ResolvedSource::Glyph { font_family, codepoint }
        }
    };
    let resolved = apply_paint(resolved, entry.paint(), color, entry.key())?;

    Ok(emit_for_target(resolved, target))
}

fn image_kind(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("png") => "Png",
        _ => "Svg",
    }
}

fn resolve_url_source(url: &str) -> Result<ResolvedSource> {
    let start = manifest_dir()?;
    let cache_path = guicons_net::url_cache_path(&start, url);
    guicons_net::ensure_cached(&cache_path, url);
    Ok(ResolvedSource::Image {
        path: cache_path.to_string_lossy().into_owned(),
        kind: "Svg",
    })
}

fn resolve_iconify_source(id: &str) -> Result<ResolvedSource> {
    let start = manifest_dir()?;
    let cache_path = guicons_net::iconify_cache_path(&start, id);
    guicons_net::ensure_cached(&cache_path, &guicons_net::iconify_url(id));
    Ok(ResolvedSource::Image {
        path: cache_path.to_string_lossy().into_owned(),
        kind: "Svg",
    })
}

fn expand_iconify_literal(id: &str, color: Option<&Expr>, target: Target) -> Result<proc_macro2::TokenStream> {
    let manifest = load_manifest(&manifest_dir()?.join("icons.gui.toml"))?;
    let resolved = resolve_iconify_source(id)?;
    let resolved = apply_paint(resolved, manifest.paint_for_iconify(id), color, id)?;
    Ok(emit_for_target(resolved, target))
}

/// Wraps a resolved source in the token stream for `target` - plain
/// `IconData` for [`Target::Data`], or a call into `guicons::slint`/
/// `guicons::iced`'s conversion helpers for the GUI-specific targets, which
/// return `Option`/panic via `.expect(..)` since the conversion itself can
/// fail (e.g. malformed PNG bytes) - that's still a compile-time-cached
/// asset, so a panic here means the cached file is corrupt, not a user
/// input problem.
fn emit_for_target(resolved: ResolvedSource, target: Target) -> proc_macro2::TokenStream {
    let data_tokens = match &resolved {
        ResolvedSource::Image { path, kind } => {
            let kind_ident = Ident::new(kind, Span::call_site());
            quote! { guicons::IconData::#kind_ident(include_bytes!(#path)) }
        }
        ResolvedSource::Painted { path, colors, color } => {
            quote! { guicons::IconData::PaintedSvg { template: include_bytes!(#path), colors: #colors, color: #color } }
        }
        ResolvedSource::Glyph { font_family, codepoint } => {
            quote! { guicons::IconData::Glyph { codepoint: #codepoint, font_family: #font_family } }
        }
    };

    match target {
        Target::Data => data_tokens,
        #[cfg(feature = "slint")]
        Target::Slint => match resolved {
            ResolvedSource::Image { .. } | ResolvedSource::Painted { .. } => quote! {
                guicons::slint::image_from_data(#data_tokens).expect("guicons: cached icon asset failed to decode")
            },
            ResolvedSource::Glyph { .. } => quote! {
                guicons::slint::glyph_from_data(#data_tokens).expect("guicons: icon entry is not a glyph")
            },
        },
        #[cfg(feature = "iced")]
        Target::Iced => match &resolved {
            ResolvedSource::Image { kind, .. } if *kind == "Png" => quote! {
                guicons::iced::image_handle_from_data(#data_tokens).expect("guicons: cached icon asset failed to decode")
            },
            ResolvedSource::Image { .. } | ResolvedSource::Painted { .. } => quote! {
                guicons::iced::svg_handle_from_data(#data_tokens).expect("guicons: cached icon asset failed to decode")
            },
            ResolvedSource::Glyph { .. } => quote! {
                guicons::iced::glyph_from_data(#data_tokens).expect("guicons: icon entry is not a glyph")
            },
        },
        #[cfg(feature = "windows-reactor")]
        Target::WindowsReactor => match resolved {
            ResolvedSource::Image { path, .. } => quote! {
                guicons::windows_reactor::icon_builder(#path)
            },
            ResolvedSource::Painted { .. } => quote! {
                guicons::windows_reactor::painted_icon_builder(#data_tokens)
            },
            ResolvedSource::Glyph { codepoint, .. } => quote! {
                guicons::windows_reactor::glyph_icon(#codepoint)
            },
        },
        #[cfg(feature = "egui")]
        Target::Egui => match resolved {
            ResolvedSource::Image { .. } | ResolvedSource::Painted { .. } => quote! {
                guicons::egui::image_source_from_data(#data_tokens).expect("guicons: cached icon asset failed to decode")
            },
            ResolvedSource::Glyph { .. } => quote! {
                guicons::egui::glyph_from_data(#data_tokens).expect("guicons: icon entry is not a glyph")
            },
        },
    }
}

fn parse_selector_literal(literal: &LitStr) -> Result<IconSelector> {
    guicons_core::selector::parse_resource_selector(&literal.value()).map_err(|message| Error::new_spanned(literal, message))
}

/// `24` in `family.24.filled` lexes as a `LitInt`, not an `Ident`, so this
/// can't just be `Punctuated<Ident, Token![.]>` - each segment is read as
/// either, then classified by the shared `classify_segments`.
fn parse_selector_path(input: ParseStream<'_>) -> Result<IconSelector> {
    let mut segments = Vec::new();
    loop {
        if input.peek(LitInt) {
            let lit: LitInt = input.parse()?;
            let value = lit
                .base10_parse::<u16>()
                .map_err(|_| Error::new_spanned(&lit, "size must fit in a u16"))?;
            segments.push(PathSegment::Size(value));
        } else {
            let ident: Ident = input.parse()?;
            segments.push(PathSegment::Ident(ident.to_string().replace('_', "-")));
        }
        if input.peek(Token![.]) {
            input.parse::<Token![.]>()?;
        } else {
            break;
        }
    }
    classify_segments(segments).map_err(|message| Error::new(Span::call_site(), message))
}

/// Nearest `icons.gui.toml` directory, walking up from `CARGO_MANIFEST_DIR` -
/// workspaces that keep one shared manifest at the root (instead of one per
/// crate) can use `icon!` from any member crate.
fn manifest_dir() -> Result<PathBuf> {
    let start = std::env::var_os("CARGO_MANIFEST_DIR")
        .ok_or_else(|| Error::new(Span::call_site(), "CARGO_MANIFEST_DIR is not set"))?;
    for dir in PathBuf::from(start).ancestors() {
        if dir.join("icons.gui.toml").exists() {
            return Ok(dir.to_path_buf());
        }
    }
    Err(Error::new(
        Span::call_site(),
        "guicons manifest not found at CARGO_MANIFEST_DIR or any ancestor",
    ))
}

fn load_manifest(path: &std::path::Path) -> Result<guicons_core::IconManifest> {
    if !path.exists() {
        return Err(Error::new(
            Span::call_site(),
            format!("guicons manifest not found at {}", path.display()),
        ));
    }

    let (manifest, errors) = guicons_core::load_icon_manifest(path);

    if !errors.is_empty() {
        let messages = errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; ");

        return Err(Error::new(
            Span::call_site(),
            format!("guicons manifest at {} has errors: {}", path.display(), messages),
        ));
    }

    Ok(manifest)
}

fn unknown_icon_message(
    manifest: &guicons_core::IconManifest,
    family: &str,
    size: Option<u16>,
    variant: Option<&str>,
) -> String {
    let mut display = family.to_string();
    if let Some(size) = size {
        display.push('/');
        display.push_str(&size.to_string());
    }
    if let Some(variant) = variant {
        display.push('/');
        display.push_str(variant);
    }

    let variants = manifest
        .entries()
        .iter()
        .filter(|entry| entry.family() == family && entry.size() == size)
        .filter_map(|entry| entry.variant())
        .collect::<Vec<_>>();

    if variants.is_empty() {
        format!("unknown guicons icon `{display}`")
    } else {
        format!(
            "unknown guicons icon `{display}`; known variants for `{family}`{}: {}",
            size.map(|size| format!("/{size}")).unwrap_or_default(),
            variants.join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse::Parser;

    fn path(input: &str) -> IconSelector {
        parse_selector_path.parse2(input.parse().unwrap()).unwrap()
    }

    fn family_variant(family: &str, size: Option<u16>, variant: Option<&str>) -> IconSelector {
        IconSelector::FamilyVariant { family: family.to_string(), size, variant: variant.map(str::to_string) }
    }

    #[test]
    fn path_selectors_parse() {
        assert_eq!(path("settings"), family_variant("settings", None, None));
        assert_eq!(path("settings.filled"), family_variant("settings", None, Some("filled")));
        assert_eq!(path("settings.24.filled"), family_variant("settings", Some(24), Some("filled")));
        assert_eq!(path("settings.20"), family_variant("settings", Some(20), None));
        assert_eq!(path("nav_bar.filled"), family_variant("nav-bar", None, Some("filled")));
    }
}

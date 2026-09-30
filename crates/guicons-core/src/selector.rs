//! Interpretation of an icon "selector" - the argument to
//! `guicons::icon!`/`icon_key!`/`icon_data!`. Tokenizing stays in
//! `guicons-macros`; this module only gives the tokens their meaning.

use winnow::ascii::alphanumeric1;
use winnow::combinator::{opt, preceded, repeat};
use winnow::token::{literal, one_of};
use winnow::{Parser, Result as WinnowResult};

/// A selector shared by `icon!` and `icon_key!`:
/// `family`/`family.variant`/`family.size.variant`/`family.size` resolves
/// against `icons.gui.toml` (the `size` segment is only needed when a
/// family has the same variant at more than one size - otherwise it's
/// redundant and can be left off); `"set:name"` (a raw iconify id)
/// resolves through `guicons-net`'s cache with no manifest lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IconSelector {
    FamilyVariant {
        family: String,
        size: Option<u16>,
        variant: Option<String>,
    },
    Iconify(String),
}

/// One dot-separated segment of the path form (`family.24.filled`) - `24`
/// is a size, everything else is an ident.
#[derive(Clone, Debug)]
pub enum PathSegment {
    Ident(String),
    Size(u16),
}

/// Interprets `[family]` / `[family, variant]` / `[family, size]` /
/// `[family, size, variant]` - a `Size` segment always comes before an
/// `Ident` (variant) segment, matching how `default_iconify_id` builds
/// `family-size-variant`.
pub fn classify_segments(segments: Vec<PathSegment>) -> Result<IconSelector, String> {
    let mut iter = segments.into_iter();
    let Some(PathSegment::Ident(family)) = iter.next() else {
        return Err("expected a family name, e.g. `settings` or `settings.filled`".to_string());
    };

    let mut size = None;
    let mut variant = None;
    for segment in iter {
        match segment {
            PathSegment::Size(value) if size.is_none() && variant.is_none() => size = Some(value),
            PathSegment::Ident(name) if variant.is_none() => variant = Some(name),
            _ => {
                return Err("expected `family`, `family.variant`, `family.size`, or `family.size.variant`".to_string());
            }
        }
    }

    Ok(IconSelector::FamilyVariant { family, size, variant })
}

/// Parses the string-literal form: `"family/variant"`, `"family/size/variant"`,
/// or a raw iconify id `"set:name"` (checked first via `contains(':')`,
/// since `:` never appears in the slash-separated form and `/` never
/// appears in an iconify id).
pub fn parse_resource_selector(input: &str) -> Result<IconSelector, String> {
    if input.contains(':') {
        return Ok(IconSelector::Iconify(input.to_string()));
    }

    let mut parser = (
        resource_segment,
        opt(preceded(literal("/"), resource_segment)),
        opt(preceded(literal("/"), resource_segment)),
    );
    let mut input_rest = input;
    let (family, second, third) = parser.parse_next(&mut input_rest).map_err(|_| {
        "expected icon selector like `settings`, `settings/filled`, or `settings/24/filled`".to_string()
    })?;
    if !input_rest.is_empty() {
        return Err(format!("unexpected trailing input `{input_rest}` in icon selector"));
    }

    let (size, variant) = match (second, third) {
        (None, None) => (None, None),
        (Some(only), None) => match only.parse::<u16>() {
            Ok(size) => (Some(size), None),
            Err(_) => (None, Some(only)),
        },
        (Some(size_segment), Some(variant)) => {
            let size = size_segment
                .parse::<u16>()
                .map_err(|_| format!("expected a numeric size before the variant, got `{size_segment}`"))?;
            (Some(size), Some(variant))
        }
        (None, Some(_)) => unreachable!("winnow's `opt` chain can't produce a third segment without a second"),
    };

    Ok(IconSelector::FamilyVariant { family, size, variant })
}

fn resource_segment(input: &mut &str) -> WinnowResult<String> {
    (
        alphanumeric1,
        repeat::<_, _, (), _, _>(0.., (one_of(['-', '_']), alphanumeric1)),
    )
        .take()
        .map(str::to_string)
        .parse_next(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(name: &str) -> PathSegment {
        PathSegment::Ident(name.to_string())
    }

    #[test]
    fn parses_a_bare_family() {
        assert_eq!(
            classify_segments(vec![ident("settings")]).unwrap(),
            IconSelector::FamilyVariant { family: "settings".to_string(), size: None, variant: None }
        );
    }

    #[test]
    fn parses_a_family_and_variant() {
        assert_eq!(
            classify_segments(vec![ident("settings"), ident("filled")]).unwrap(),
            IconSelector::FamilyVariant {
                family: "settings".to_string(),
                size: None,
                variant: Some("filled".to_string())
            }
        );
    }

    #[test]
    fn parses_a_family_size_and_variant() {
        assert_eq!(
            classify_segments(vec![ident("settings"), PathSegment::Size(24), ident("filled")]).unwrap(),
            IconSelector::FamilyVariant {
                family: "settings".to_string(),
                size: Some(24),
                variant: Some("filled".to_string())
            }
        );
    }

    #[test]
    fn a_size_after_a_variant_is_rejected() {
        assert!(classify_segments(vec![ident("settings"), ident("filled"), PathSegment::Size(24)]).is_err());
    }

    #[test]
    fn empty_input_is_rejected() {
        assert!(classify_segments(Vec::new()).is_err());
    }

    #[test]
    fn parses_a_slash_separated_family_and_variant() {
        assert_eq!(
            parse_resource_selector("settings/filled").unwrap(),
            IconSelector::FamilyVariant {
                family: "settings".to_string(),
                size: None,
                variant: Some("filled".to_string())
            }
        );
    }

    #[test]
    fn parses_a_slash_separated_family_size_and_variant() {
        assert_eq!(
            parse_resource_selector("settings/24/filled").unwrap(),
            IconSelector::FamilyVariant {
                family: "settings".to_string(),
                size: Some(24),
                variant: Some("filled".to_string())
            }
        );
    }

    #[test]
    fn a_colon_makes_it_an_iconify_id_regardless_of_slashes() {
        assert_eq!(parse_resource_selector("mdi:home").unwrap(), IconSelector::Iconify("mdi:home".to_string()));
    }
}

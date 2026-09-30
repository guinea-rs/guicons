use super::shared::write_if_changed;
use crate::materialize::{ImageKind, MaterializedIcon, MaterializedIconBackend, MaterializedPaint};
use guicons_core::{rust_const_name, rust_fn_name, rust_variant_name, PaintColor};
use std::collections::BTreeMap;
use std::path::Path;

pub(crate) fn generate_rust_icon_registry_from_materialized(
    manifest_path: &Path,
    out_file: &Path,
    icons: &[MaterializedIcon],
    slint_image_resolver: bool,
    windows_reactor_image_resolver: bool,
    egui_image_resolver: bool,
) {
    let key_consts = icons
        .iter()
        .map(|icon| {
            format!(
                "    pub const {}: IconKey = IconKey::new(\"{}\");",
                rust_const_name(&icon.key),
                icon.key
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let all_keys = icons
        .iter()
        .map(|icon| format!("keys::{}", rust_const_name(&icon.key)))
        .collect::<Vec<_>>()
        .join(", ");
    let key_from_name_arms = icons
        .iter()
        .map(|icon| format!("        \"{}\" => Some(keys::{}),", icon.key, rust_const_name(&icon.key)))
        .collect::<Vec<_>>()
        .join("\n");
    let name_for_key_arms = icons
        .iter()
        .map(|icon| format!("        keys::{} => Some(\"{}\"),", rust_const_name(&icon.key), icon.key))
        .collect::<Vec<_>>()
        .join("\n");

    let families = unique_strings(icons.iter().map(|icon| icon.family.as_str()));
    let variants = unique_strings(icons.iter().filter_map(|icon| icon.variant.as_deref()));
    let family_consts = families
        .iter()
        .map(|family| format!("    pub const {}: IconFamily = IconFamily::new(\"{}\");", rust_const_name(family), family))
        .collect::<Vec<_>>()
        .join("\n");
    let variant_consts = variants
        .iter()
        .map(|variant| format!("    pub const {}: IconVariant = IconVariant::new(\"{}\");", rust_const_name(variant), variant))
        .collect::<Vec<_>>()
        .join("\n");
    // Keyed on (family, size, variant) together, not just (family, variant):
    // a family can repeat the same variant at more than one size (see
    // `generate_builders`), and matching on family+variant alone would
    // produce unreachable, silently-wrong duplicate match arms for those.
    let family_variant_arms = icons
        .iter()
        .map(|icon| {
            let size_pattern = rust_option_pattern(icon.size.map(|size| size.to_string()));
            let variant_pattern =
                rust_option_pattern(icon.variant.as_ref().map(|variant| format!("variants::{}", rust_const_name(variant))));
            format!(
                "        (families::{}, {size_pattern}, {variant_pattern}) => Some(keys::{}),",
                rust_const_name(&icon.family),
                rust_const_name(&icon.key)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let dynamic_family_variant_arms = icons
        .iter()
        .map(|icon| {
            let size_pattern = rust_option_pattern(icon.size.map(|size| size.to_string()));
            let variant_pattern = rust_option_pattern(icon.variant.as_ref().map(|variant| format!("\"{variant}\"")));
            format!(
                "        (\"{}\", {size_pattern}, {variant_pattern}) => Some(keys::{}),",
                icon.family,
                rust_const_name(&icon.key)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let data_arms = icons
        .iter()
        .map(|icon| match &icon.backend {
            MaterializedIconBackend::Image { paint: Some(MaterializedPaint { template, colors, .. }), .. } if !icon.dynamic => {
                let template = template.to_string_lossy().replace('\\', "\\\\");
                let rgb = |color: PaintColor| format!("guicons::Color::rgb({}, {}, {})", color.r, color.g, color.b);
                format!(
                    "        keys::{} => Some(IconData::PaintedSvg {{ template: include_bytes!(\"{template}\"), colors: guicons::ThemeColors {{ light: {}, dark: {} }}, color: None }}),",
                    rust_const_name(&icon.key),
                    rgb(colors.light),
                    rgb(colors.dark)
                )
            }
            MaterializedIconBackend::Image { path, kind, .. } if !icon.dynamic => {
                let path = path.to_string_lossy().replace('\\', "\\\\");
                let ctor = match kind {
                    ImageKind::Svg => "Svg",
                    ImageKind::Png => "Png",
                };
                format!(
                    "        keys::{} => Some(IconData::{ctor}(include_bytes!(\"{path}\"))),",
                    rust_const_name(&icon.key)
                )
            }
            MaterializedIconBackend::Glyph { font_family, codepoint } => {
                format!(
                    "        keys::{} => Some(IconData::Glyph {{ codepoint: {}, font_family: \"{}\" }}),",
                    rust_const_name(&icon.key),
                    char_literal(*codepoint),
                    escape_rust_string(font_family)
                )
            }
            _ => format!("        keys::{} => None,", rust_const_name(&icon.key)),
        })
        .collect::<Vec<_>>()
        .join("\n");

    let builders = generate_builders(icons);

    let slint_resolver = if slint_image_resolver {
        "\npub fn resolve_image<'a>(icon: impl Into<IconRef<'a>>) -> slint::Image {\n    resolve(icon).and_then(guicons::slint::image_from_data).unwrap_or_default()\n}\n"
    } else {
        ""
    };

    let windows_reactor_resolver = if windows_reactor_image_resolver {
        "\npub fn resolve_image<'a>(icon: impl Into<IconRef<'a>>) -> windows_reactor::Image {\n    resolve(icon).map(guicons::windows_reactor::image_from_data).unwrap_or_default()\n}\n"
    } else {
        ""
    };

    let egui_resolver = if egui_image_resolver {
        "\npub fn resolve_image_source<'a>(icon: impl Into<IconRef<'a>>) -> Option<egui::ImageSource<'static>> {\n    resolve(icon).and_then(guicons::egui::image_source_from_data)\n}\n"
    } else {
        ""
    };

    let generated = format!(
        r#"// AUTO-GENERATED from {}
use guicons::{{IconData, IconFamily, IconKey, IconRef, IconVariant}};

pub mod keys {{
    use super::IconKey;

{key_consts}
}}

pub mod families {{
    use super::IconFamily;

{family_consts}
}}

pub mod variants {{
    use super::IconVariant;

{variant_consts}
}}

{builders}
pub const ALL_KEYS: &[IconKey] = &[{all_keys}];

pub fn name_for_key(key: IconKey) -> Option<&'static str> {{
    match key {{
{name_for_key_arms}
        _ => None,
    }}
}}

pub fn key_from_name(name: &str) -> Option<IconKey> {{
    match name {{
{key_from_name_arms}
        _ => None,
    }}
}}

pub fn key_from_family_variant(family: IconFamily, size: Option<u16>, variant: Option<IconVariant>) -> Option<IconKey> {{
    match (family, size, variant) {{
{family_variant_arms}
        _ => None,
    }}
}}

pub fn key_from_dynamic_family_variant(family: &str, size: Option<u16>, variant: Option<&str>) -> Option<IconKey> {{
    match (family, size, variant) {{
{dynamic_family_variant_arms}
        _ => None,
    }}
}}

pub fn key_from_ref(icon: IconRef<'_>) -> Option<IconKey> {{
    match icon {{
        IconRef::Key(key) => Some(key),
        IconRef::Name(name) => key_from_name(name),
        IconRef::FamilyVariant {{ family, size, variant }} => key_from_family_variant(family, size, variant),
        IconRef::DynamicFamilyVariant {{ family, size, variant }} => key_from_dynamic_family_variant(family, size, variant),
    }}
}}

pub fn data_for(key: IconKey) -> Option<IconData> {{
    match key {{
{data_arms}
        _ => None,
    }}
}}

pub fn resolve<'a>(icon: impl Into<IconRef<'a>>) -> Option<IconData> {{
    key_from_ref(icon.into()).and_then(data_for)
}}
{slint_resolver}{windows_reactor_resolver}{egui_resolver}"#,
        manifest_file_name(manifest_path)
    );

    write_if_changed(out_file, &generated);
}

pub(super) fn generate_builders(icons: &[MaterializedIcon]) -> String {
    let mut families: BTreeMap<&str, Vec<&MaterializedIcon>> = BTreeMap::new();
    for icon in icons {
        families.entry(icon.family.as_str()).or_default().push(icon);
    }

    families
        .into_iter()
        .map(|(family, members)| generate_family_builder(family, &members))
        .collect::<Vec<_>>()
        .join("\n")
}

fn generate_family_builder(family: &str, members: &[&MaterializedIcon]) -> String {
    let mut unsized_icons: Vec<&MaterializedIcon> = Vec::new();
    let mut sized: BTreeMap<u16, Vec<&MaterializedIcon>> = BTreeMap::new();
    for icon in members {
        match icon.size {
            Some(size) => sized.entry(size).or_default().push(*icon),
            None => unsized_icons.push(icon),
        }
    }

    let fn_name = rust_fn_name(family);
    let type_name = rust_variant_name(family);

    if sized.is_empty() {
        return generate_leaf(&fn_name, &format!("{type_name}Builder"), &unsized_icons);
    }

    let builder_type = format!("{type_name}Builder");
    let mut methods = generate_group_methods(&unsized_icons);
    let mut nested_types = String::new();

    for (size, group) in &sized {
        if group.len() == 1 && group[0].variant.is_none() {
            methods.push_str(&format!(
                "    pub const fn size_{size}(self) -> IconKey {{\n        keys::{}\n    }}\n",
                rust_const_name(&group[0].key)
            ));
            continue;
        }

        let size_type = format!("{type_name}{size}VariantBuilder");
        let variant_methods = generate_group_methods(group);
        nested_types.push_str(&format!("pub struct {size_type};\n\nimpl {size_type} {{\n{variant_methods}}}\n\n"));
        methods.push_str(&format!("    pub const fn size_{size}(self) -> {size_type} {{\n        {size_type}\n    }}\n"));
    }

    format!(
        "{nested_types}pub struct {builder_type};\n\npub const fn {fn_name}() -> {builder_type} {{\n    {builder_type}\n}}\n\nimpl {builder_type} {{\n{methods}}}\n"
    )
}

fn generate_leaf(fn_name: &str, builder_type: &str, icons: &[&MaterializedIcon]) -> String {
    if let [icon] = icons {
        if icon.variant.is_none() {
            return format!("pub const fn {fn_name}() -> IconKey {{\n    keys::{}\n}}\n", rust_const_name(&icon.key));
        }
    }

    let methods = generate_group_methods(icons);
    format!(
        "pub struct {builder_type};\n\npub const fn {fn_name}() -> {builder_type} {{\n    {builder_type}\n}}\n\nimpl {builder_type} {{\n{methods}}}\n"
    )
}

fn generate_group_methods(icons: &[&MaterializedIcon]) -> String {
    icons
        .iter()
        .map(|icon| {
            let method_name = match icon.variant.as_deref() {
                Some(variant) => rust_fn_name(variant),
                None => "default".to_string(),
            };
            format!(
                "    pub const fn {method_name}(self) -> IconKey {{\n        keys::{}\n    }}\n",
                rust_const_name(&icon.key)
            )
        })
        .collect()
}

fn manifest_file_name(manifest_path: &Path) -> String {
    manifest_path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| manifest_path.display().to_string())
}

fn rust_option_pattern(inner: Option<String>) -> String {
    match inner {
        Some(inner) => format!("Some({inner})"),
        None => "None".to_string(),
    }
}

fn unique_strings<'a>(values: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    let mut values = values.collect::<Vec<_>>();
    values.sort();
    values.dedup();
    values
}

fn char_literal(ch: char) -> String {
    format!("{ch:?}")
}

fn escape_rust_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

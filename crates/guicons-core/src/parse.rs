//! Turns one already-parsed `toml_span` document into `IconEntry` values.
//!
//! Nothing here touches the filesystem or knows about `[link]` — that's
//! [`crate::load`]'s job. This module only walks the table tree produced by
//! `toml_span::parse` and validates/extracts entries from it.

use crate::diagnostics::Diagnostics;
use crate::model::{
    IconEntry, IconEntrySource, IconManifest, ImageFormat, ManifestDefaults, Paint, PaintColor, ProviderSchema, ThemePaint,
};
use crate::paths::resolve_workspace_path;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use toml_span::de_helpers::TableHelper;
use toml_span::value::{Key, Table, Value, ValueInner};
use toml_span::{Span, Spanned};

pub(crate) const ENTRY_KEYS: &[&str] = &[
    "file",
    "iconify",
    "url",
    "glyph",
    "windows-ico",
    "dynamic",
    "root",
];
pub(crate) const RESERVED_TOP_LEVEL: &[&str] = &["defaults", "link", "providers"];

fn take_table<'de>(value: &mut Value<'de>) -> Option<Table<'de>> {
    match value.take() {
        ValueInner::Table(table) => Some(table),
        _ => None,
    }
}

fn take_paint(table: &mut Table<'_>, diags: &mut Diagnostics) -> Option<Paint> {
    parse_paint(table.remove("paint")?, diags).ok()
}

/// `Err` once the error is reported, so the caller can drop the whole table.
fn parse_optional_paint(value: Option<(Key<'_>, Value<'_>)>, diags: &mut Diagnostics) -> Result<Option<Paint>, ()> {
    value.map(|(_, value)| parse_paint(value, diags)).transpose()
}

/// `Err` once the error is reported.
fn parse_paint(mut value: Value<'_>, diags: &mut Diagnostics) -> Result<Paint, ()> {
    let span = value.span;
    match value.take() {
        ValueInner::String(text) => Paint::parse(&text).map_err(|message| diags.error(Some(span.into()), message)),
        ValueInner::Table(table) => {
            let mut th = TableHelper::from((table, span));
            let light: Option<Spanned<String>> = th.optional_s("light");
            let dark: Option<Spanned<String>> = th.optional_s("dark");
            if let Err(err) = th.finalize(None) {
                diags.push_deser_error(err);
                return Err(());
            }
            let (Some(light), Some(dark)) = (light, dark) else {
                diags.error(Some(span.into()), "`paint` per theme needs both `light` and `dark`");
                return Err(());
            };
            Ok(Paint::Colors(ThemePaint { light: parse_color(light, diags)?, dark: parse_color(dark, diags)? }))
        }
        _ => {
            diags.error(
                Some(span.into()),
                "`paint` must be `\"none\"`, a `#rgb`/`#rrggbb` color, or `{ light = ..., dark = ... }`",
            );
            Err(())
        }
    }
}

fn parse_color(value: Spanned<String>, diags: &mut Diagnostics) -> Result<PaintColor, ()> {
    PaintColor::parse(&value.value).map_err(|message| diags.error(Some(value.span.into()), message))
}

fn split_family_and_size(path: &[String]) -> (String, Option<u16>) {
    if let Some((last, rest)) = path.split_last() {
        if let Ok(size) = last.parse::<u16>() {
            return (rest.join("-"), Some(size));
        }
    }
    (path.join("-"), None)
}

pub(crate) fn collect_entries(
    path: Vec<String>,
    mut table: Table<'_>,
    workspace_root: &Path,
    defaults: &ManifestDefaults,
    providers: &HashMap<String, ProviderSchema>,
    inherited_paint: Option<Paint>,
    diags: &mut Diagnostics,
    acc: &mut Vec<IconEntry>,
) {
    // A flat `[fluent]` family at the manifest's top level and a provider
    // schema override (`[providers.fluent]`) collide on name but not on
    // shape - a schema's `variants = [...]` is a plain string array,
    // while an icon family's `variants` table (just below) is inline
    // tables keyed by variant name. Getting this wrong used to surface as
    // a confusing "`variants` in `fluent` must be a table" parse error
    // (true, but not the actual mistake) - almost certainly the user
    // meant the provider-schema section and just wrote the wrong path,
    // not that they want an icon family named identically to a real
    // iconify provider. Only checked at the top level (`path.len() == 1`)
    // - `[settings.fluent]` naming a *variant* "fluent" is unambiguous
    // and unrelated to this collision.
    if path.len() == 1 && crate::iconify_providers::is_known_iconify_provider_prefix(&path[0]) {
        let name = &path[0];
        diags.error(
            Some(table_span(&table).into()),
            format!("`[{name}]` is a reserved iconify provider name - did you mean `[providers.{name}]`?"),
        );
        return;
    }

    if let Some(mut variants_value) = table.remove("variants") {
        let inherited_paint = take_paint(&mut table, diags).or(inherited_paint);
        let key_prefix = path.join("-");
        let (family, explicit_size) = split_family_and_size(&path);
        let variants_span = variants_value.span;
        let Some(variants_table) = take_table(&mut variants_value) else {
            diags.error(
                Some(variants_span.into()),
                format!("`variants` in `{key_prefix}` must be a table"),
            );
            return;
        };

        for (variant_key, mut variant_value) in variants_table {
            let variant = variant_key.name.to_string();
            let entry_span = variant_value.span;
            if !check_name(&variant, variant_key.span, diags) {
                continue;
            }
            match take_table(&mut variant_value) {
                Some(variant_table) => {
                    if let Some(entry) = parse_entry(
                        format!("{key_prefix}-{variant}"),
                        family.clone(),
                        Some(variant),
                        variant_table,
                        entry_span,
                        workspace_root,
                        defaults,
                        providers,
                        explicit_size,
                        inherited_paint,
                        diags,
                    ) {
                        acc.push(entry);
                    }
                }
                None => diags.error(
                    Some(entry_span.into()),
                    format!("variant `{key_prefix}.{variant}` must be an inline table"),
                ),
            }
        }
        collect_groups(path, table, workspace_root, defaults, providers, inherited_paint, diags, acc);
        return;
    }

    let is_entry = table
        .keys()
        .any(|key| ENTRY_KEYS.contains(&key.name.as_ref()));
    if is_entry {
        let key = path.join("-");
        let (family, explicit_size) = split_family_and_size(&path);
        let table_span = table_span(&table);
        if let Some(entry) = parse_entry(
            key,
            family,
            None,
            table,
            table_span,
            workspace_root,
            defaults,
            providers,
            explicit_size,
            inherited_paint,
            diags,
        ) {
            acc.push(entry);
        }
        return;
    }

    let inherited_paint = if path.is_empty() {
        inherited_paint
    } else {
        take_paint(&mut table, diags).or(inherited_paint)
    };
    collect_groups(path, table, workspace_root, defaults, providers, inherited_paint, diags, acc);
}

#[allow(clippy::too_many_arguments)]
fn collect_groups(
    path: Vec<String>,
    table: Table<'_>,
    workspace_root: &Path,
    defaults: &ManifestDefaults,
    providers: &HashMap<String, ProviderSchema>,
    inherited_paint: Option<Paint>,
    diags: &mut Diagnostics,
    acc: &mut Vec<IconEntry>,
) {
    for (key, mut value) in table {
        let key_name = key.name.to_string();
        if path.is_empty() && RESERVED_TOP_LEVEL.contains(&key_name.as_str()) {
            continue;
        }
        let value_span = value.span;
        let Some(sub_table) = take_table(&mut value) else {
            diags.error(
                Some(value_span.into()),
                format!("unexpected value at icon manifest group {path:?}: `{key_name}`"),
            );
            continue;
        };
        if !check_name(&key_name, key.span, diags) {
            continue;
        }
        let mut next_path = path.clone();
        next_path.push(key_name);
        collect_entries(next_path, sub_table, workspace_root, defaults, providers, inherited_paint, diags, acc);
    }
}

/// Icon names end up in generated Rust and Slint identifiers and strings.
fn check_name(name: &str, span: Span, diags: &mut Diagnostics) -> bool {
    let valid = !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !valid {
        diags.error(Some(span.into()), format!("icon name `{name}` may only contain ASCII letters, digits, `-` and `_`"));
    }
    valid
}

/// Folds over *keys* as well as values - a table this covers only by its
/// values' spans (the original bug: `IconEntry::span()` exists precisely
/// so editor tooling can map a cursor position back to an entry, but a
/// single-key flat entry like `[family]\nfile = "x.svg"` produced a span
/// covering only the `"x.svg"` string literal, not the `file` key text
/// right next to it - the single most natural place to actually click).
/// An inline `variants.name = { ... }` table never had this problem since
/// its *value* span (an inline table's own `{ ... }` span) already
/// includes every key inside it - only a real, non-inline table (this
/// function's actual input) needed the fix. Still doesn't reach back to
/// cover the `[section]`/`[section.sub]` header line itself, since by the
/// time this runs the table's own wrapping `Value` (which carried that)
/// has already been unwrapped by the caller - a real gap, just a smaller
/// and much less commonly clicked one than the key/value bug this fixes.
fn table_span(table: &Table<'_>) -> Span {
    table.iter().fold(Span::new(usize::MAX, 0), |acc, (key, value)| {
        Span::new(acc.start.min(key.span.start), acc.end.max(value.span.end))
    })
}

fn parse_entry(
    key: String,
    family: String,
    variant: Option<String>,
    table: Table<'_>,
    table_span: Span,
    workspace_root: &Path,
    defaults: &ManifestDefaults,
    providers: &HashMap<String, ProviderSchema>,
    explicit_size: Option<u16>,
    inherited_paint: Option<Paint>,
    diags: &mut Diagnostics,
) -> Option<IconEntry> {
    let mut th = TableHelper::from((table, table_span));

    let has_file = th.contains("file");
    let has_iconify = th.contains("iconify");
    let has_url = th.contains("url");
    let has_glyph = th.contains("glyph");

    let root: Option<String> = th.optional("root");
    let file: Option<String> = th.optional("file");
    let iconify: Option<String> = th.optional("iconify");
    let url: Option<String> = th.optional("url");
    let glyph: Option<String> = th.optional("glyph");
    let windows_ico: Option<String> = th.optional("windows-ico");
    let dynamic: bool = th.optional("dynamic").unwrap_or(false);
    let own_paint = th.take("paint");

    if let Err(err) = th.finalize(None) {
        diags.push_deser_error(err);
        return None;
    }

    let own_paint_span = own_paint.as_ref().map(|(_, paint)| paint.span);
    let own_paint = parse_optional_paint(own_paint, diags).ok()?;

    let roots: Vec<PathBuf> = root
        .map(|value| vec![resolve_workspace_path(workspace_root, &value)])
        .unwrap_or_else(|| defaults.roots.clone());

    let resolved_size = explicit_size.or(defaults.size);

    let default_iconify = if has_file || has_iconify || has_url || has_glyph {
        None
    } else {
        default_iconify_id(&family, variant.as_deref(), resolved_size, defaults)
    };
    let iconify = iconify.or_else(|| default_iconify.clone());

    let explicit_source_count = [has_file, has_iconify, has_url, has_glyph]
        .into_iter()
        .filter(|present| *present)
        .count();
    let source_count = explicit_source_count + usize::from(default_iconify.is_some());

    if source_count != 1 {
        diags.error(
            Some(table_span.into()),
            format!(
                "icon manifest entry `{key}` must define exactly one source (file/iconify/url/glyph), found {source_count}"
            ),
        );
        return None;
    }

    let source = if let Some(path) = file {
        IconEntrySource::File(resolve_file_from_roots(&roots, &path))
    } else if let Some(id) = iconify {
        IconEntrySource::Iconify(id)
    } else if let Some(url) = url {
        IconEntrySource::Url(url)
    } else if let Some(glyph) = glyph {
        if let Err(reason) = try_parse_glyph_spec(&glyph) {
            diags.error(
                Some(table_span.into()),
                format!("icon manifest entry `{key}` has an invalid glyph spec: {reason}"),
            );
            return None;
        }
        IconEntrySource::Glyph(glyph)
    } else {
        diags.error(
            Some(table_span.into()),
            format!("icon manifest entry `{key}` has no usable source"),
        );
        return None;
    };

    // Only the *auto-built* id (from `defaults.provider` + this entry's own
    // `variant`/`size`) is worth validating against the provider's schema -
    // an explicitly-written `iconify = "..."` string is deliberately
    // unconstrained (real manifests reference dozens of iconify
    // collections - `mdi`, `tabler`, `fa`, ... - with no `[providers.*]`
    // declared for most of them at all, since a schema is only needed for
    // auto-building/decomposing ids, not for using one verbatim). But if
    // `defaults.provider` itself doesn't resolve to a known provider, or
    // this entry's variant/size aren't among the schema it does resolve to,
    // the id being silently built is one nothing will ever find.
    if default_iconify.is_some() {
        if let IconEntrySource::Iconify(id) = &source {
            let provider_name = defaults.provider.as_deref().expect("default_iconify_id only builds an id when defaults.provider is set");
            match providers.get(provider_name) {
                None => {
                    diags.error(
                        Some(table_span.into()),
                        format!(
                            "icon manifest entry `{key}` auto-builds iconify id `{id}` from `defaults.provider = \"{provider_name}\"`, but `{provider_name}` isn't a known provider (builtin or declared in `[providers]`)"
                        ),
                    );
                    return None;
                }
                Some(schema) => {
                    if let Some(variant_name) = variant.as_deref() {
                        if !schema.variants.is_empty() && !schema.variants.iter().any(|known| known == variant_name) {
                            diags.error(
                                Some(table_span.into()),
                                format!(
                                    "icon manifest entry `{key}` has variant `{variant_name}`, not declared in `[providers.{provider_name}]`'s variants ({})",
                                    schema.variants.join(", ")
                                ),
                            );
                            return None;
                        }
                    }
                    if let Some(size) = resolved_size {
                        if !schema.sizes.is_empty() && !schema.sizes.contains(&size) {
                            diags.error(
                                Some(table_span.into()),
                                format!(
                                    "icon manifest entry `{key}` has size `{size}`, not declared in `[providers.{provider_name}]`'s sizes ({})",
                                    schema.sizes.iter().map(u16::to_string).collect::<Vec<_>>().join(", ")
                                ),
                            );
                            return None;
                        }
                    }
                }
            }
        }
    }

    let file_format = match &source {
        IconEntrySource::File(path) => match ImageFormat::of_file(path) {
            Some(format) => Some(format),
            None if path.exists() || path.extension().is_some_and(|ext| !ext.eq_ignore_ascii_case("svg")) => {
                diags.error(
                    Some(table_span.into()),
                    format!("icon manifest entry `{key}` points at `{}`, which is neither SVG nor PNG", path.display()),
                );
                return None;
            }
            None => Some(ImageFormat::Svg),
        },
        _ => None,
    };
    let paintable = match &source {
        IconEntrySource::File(_) => file_format != Some(ImageFormat::Png),
        IconEntrySource::Iconify(_) | IconEntrySource::Url(_) => true,
        IconEntrySource::Glyph(_) => false,
    };
    let paint = if paintable {
        let provider_paint = match &source {
            IconEntrySource::Iconify(id) => id
                .split_once(':')
                .and_then(|(provider, _)| providers.get(provider))
                .and_then(|schema| schema.paint),
            _ => None,
        };
        own_paint
            .or(inherited_paint)
            .or(provider_paint)
            .or(defaults.paint)
            .and_then(Paint::colors)
    } else {
        if let (Some(Paint::Colors(_)), Some(span)) = (own_paint, own_paint_span) {
            diags.error(
                Some(span.into()),
                format!("icon manifest entry `{key}` can't be painted: `paint` only recolors SVG icons"),
            );
            return None;
        }
        None
    };

    let windows_ico = windows_ico.map(|value| resolve_file_from_roots(&roots, &value));

    Some(IconEntry {
        key,
        family,
        variant,
        size: resolved_size,
        source,
        dynamic,
        windows_ico,
        paint,
        span: table_span.into(),
        // Filled in by `load`, which is the only layer that knows which
        // file's table tree is currently being walked.
        file: PathBuf::new(),
    })
}

/// A `[link]`ed file inherits the including file's defaults (`inherited`);
/// each key it declares itself wins. Without a root of its own it searches
/// the inherited roots first, then its own directory and the workspace root.
pub(crate) fn parse_defaults(
    defaults_value: Option<Value<'_>>,
    workspace_root: &Path,
    manifest_dir: &Path,
    inherited: Option<&ManifestDefaults>,
    diags: &mut Diagnostics,
) -> ManifestDefaults {
    let own = parse_own_defaults(defaults_value, workspace_root, diags);
    let inherited = inherited.cloned().unwrap_or_default();
    let fallback = [manifest_dir.to_path_buf(), workspace_root.to_path_buf()];
    let (roots, declared_roots) = if !own.roots.is_empty() {
        (own.roots.clone(), own.roots)
    } else {
        let declared = inherited.declared_roots;
        (declared.iter().cloned().chain(fallback).collect(), declared)
    };
    ManifestDefaults {
        roots,
        declared_roots,
        provider: own.provider.or(inherited.provider),
        size: own.size.or(inherited.size),
        paint: own.paint.or(inherited.paint),
    }
}

/// Only what `[defaults]` itself declares; `roots` is empty when it
/// declares none.
fn parse_own_defaults(defaults_value: Option<Value<'_>>, workspace_root: &Path, diags: &mut Diagnostics) -> ManifestDefaults {
    let Some(mut defaults_value) = defaults_value else {
        return ManifestDefaults::default();
    };
    let span = defaults_value.span;
    let Some(table) = take_table(&mut defaults_value) else {
        diags.error(Some(span.into()), "`[defaults]` must be a table");
        return ManifestDefaults::default();
    };

    let mut th = TableHelper::from((table, span));
    let root: Option<String> = th.optional("root");
    let roots_field: Option<Vec<String>> = th.optional("roots");
    let provider: Option<String> = th.optional("provider");
    let size: Option<u16> = th.optional("size");
    let paint = th.take("paint");
    if let Err(err) = th.finalize(None) {
        diags.push_deser_error(err);
    }
    let paint = parse_optional_paint(paint, diags).ok().flatten();

    let mut roots = Vec::new();
    if let Some(value) = root {
        roots.push(resolve_workspace_path(workspace_root, &value));
    }
    if let Some(values) = roots_field {
        roots.extend(values.iter().map(|value| resolve_workspace_path(workspace_root, value)));
    }

    ManifestDefaults {
        roots,
        provider,
        size,
        paint,
        ..ManifestDefaults::default()
    }
}

/// A single `[providers.<name>]` (or `[providers.<name>.override]`) entry,
/// before it's checked against the builtin set - that check needs
/// `resolve_providers`, which knows about builtins; this only knows what
/// shape the TOML was in.
pub(crate) enum ProviderDeclaration {
    Full { schema: ProviderSchema, span: toml_span::Span },
    Override {
        variants: Option<Vec<String>>,
        sizes: Option<Vec<u16>>,
        paint: Option<Paint>,
        span: toml_span::Span,
    },
}

pub(crate) fn parse_providers(
    providers_value: Option<Value<'_>>,
    diags: &mut Diagnostics,
) -> HashMap<String, ProviderDeclaration> {
    let Some(mut providers_value) = providers_value else {
        return HashMap::new();
    };
    let span = providers_value.span;
    let Some(table) = take_table(&mut providers_value) else {
        diags.error(Some(span.into()), "`[providers]` must be a table");
        return HashMap::new();
    };

    let mut providers = HashMap::new();
    for (name_key, mut value) in table {
        let name = name_key.name.to_string();
        let entry_span = value.span;
        let Some(mut entry_table) = take_table(&mut value) else {
            diags.error(
                Some(entry_span.into()),
                format!("`providers.{name}` must be a table"),
            );
            continue;
        };

        if let Some(mut override_value) = entry_table.remove("override") {
            if !entry_table.is_empty() {
                diags.error(
                    Some(entry_span.into()),
                    format!(
                        "`providers.{name}` can't mix its own fields with `providers.{name}.override` - use one or the other"
                    ),
                );
                continue;
            }
            let override_span = override_value.span;
            let Some(override_table) = take_table(&mut override_value) else {
                diags.error(Some(override_span.into()), format!("`providers.{name}.override` must be a table"));
                continue;
            };

            let mut th = TableHelper::from((override_table, override_span));
            let variants: Option<Vec<String>> = th.optional("variants");
            let sizes: Option<Vec<u16>> = th.optional("sizes");
            let paint = th.take("paint");
            if let Err(err) = th.finalize(None) {
                diags.push_deser_error(err);
                continue;
            }
            let Ok(paint) = parse_optional_paint(paint, diags) else {
                continue;
            };

            providers.insert(name, ProviderDeclaration::Override { variants, sizes, paint, span: entry_span });
            continue;
        }

        let mut th = TableHelper::from((entry_table, entry_span));
        let variants: Option<Vec<String>> = th.optional("variants");
        let sizes: Option<Vec<u16>> = th.optional("sizes");
        let paint = th.take("paint");
        if let Err(err) = th.finalize(None) {
            diags.push_deser_error(err);
            continue;
        }
        let Ok(paint) = parse_optional_paint(paint, diags) else {
            continue;
        };

        providers.insert(
            name,
            ProviderDeclaration::Full {
                schema: ProviderSchema {
                    variants: variants.unwrap_or_default(),
                    sizes: sizes.unwrap_or_default(),
                    paint,
                },
                span: entry_span,
            },
        );
    }
    providers
}

fn builtin_providers() -> &'static HashMap<String, ProviderSchema> {
    static BUILTIN: std::sync::OnceLock<HashMap<String, ProviderSchema>> = std::sync::OnceLock::new();
    BUILTIN.get_or_init(|| {
        const SOURCE: &str = include_str!("builtin_providers.gui.toml");
        let root = toml_span::parse(SOURCE).expect("builtin_providers.gui.toml is valid TOML");
        let mut errors = Vec::new();
        let mut diags = Diagnostics {
            file: Path::new("<builtin_providers.gui.toml>"),
            errors: &mut errors,
        };
        let declarations = parse_providers(Some(root), &mut diags);
        assert!(errors.is_empty(), "builtin_providers.gui.toml failed to parse: {errors:?}");

        declarations
            .into_iter()
            .map(|(name, declaration)| match declaration {
                ProviderDeclaration::Full { schema, .. } => (name, schema),
                ProviderDeclaration::Override { .. } => {
                    unreachable!("builtin_providers.gui.toml can't `.override` itself")
                }
            })
            .collect()
    })
}

pub fn builtin_provider_names() -> impl Iterator<Item = &'static str> {
    builtin_providers().keys().map(String::as_str)
}

/// Resolves one manifest's `[providers.*]` declarations against the builtin
/// set: a bare `[providers.X]` redefining a builtin name is rejected (use
/// `.override` instead, so it's clear which one was meant); `.override`
/// fields replace the builtin's per-field, inheriting whatever it didn't
/// specify; builtins the file didn't mention at all pass through unchanged.
pub(crate) fn resolve_providers(
    declarations: HashMap<String, ProviderDeclaration>,
    diags: &mut Diagnostics,
) -> HashMap<String, ProviderSchema> {
    let builtin = builtin_providers();
    let mut resolved = HashMap::new();

    for (name, declaration) in declarations {
        match declaration {
            ProviderDeclaration::Full { schema, span } => {
                if builtin.contains_key(&name) {
                    diags.error(
                        Some(span.into()),
                        format!(
                            "`providers.{name}` is a built-in provider; use `[providers.{name}.override]` to customize it instead of redefining it"
                        ),
                    );
                    continue;
                }
                resolved.insert(name, schema);
            }
            ProviderDeclaration::Override { variants, sizes, paint, span } => match builtin.get(&name) {
                Some(base) => {
                    resolved.insert(
                        name,
                        ProviderSchema {
                            variants: variants.unwrap_or_else(|| base.variants.clone()),
                            sizes: sizes.unwrap_or_else(|| base.sizes.clone()),
                            paint: paint.or(base.paint),
                        },
                    );
                }
                None => {
                    diags.error(
                        Some(span.into()),
                        format!("`providers.{name}.override` used, but `{name}` isn't a built-in provider"),
                    );
                }
            },
        }
    }

    for (name, schema) in builtin {
        resolved.entry(name.clone()).or_insert_with(|| schema.clone());
    }

    resolved
}

/// Reverses `default_iconify_id`: given a raw iconify id and a provider
/// schema declared in the manifest, splits it back into `family`, `size`,
/// and `variant`. Returns `None` if the provider has no `[providers.X]`
/// entry - callers decide what to fall back to (e.g. ask for `--name`).
///
/// Icon set naming conventions vary too much to assume fixed positions:
/// Fluent always has both `-size-variant`, Phosphor has no size and its
/// bare name is unsuffixed, Material Symbols stacks multiple suffix tokens.
/// So suffixes are matched by membership in `schema.variants`/`schema.sizes`,
/// not by position, and the variant suffix match is greedy-longest so a
/// multi-segment variant like `outline-rounded` isn't torn apart.
pub fn decompose_iconify_id(
    id: &str,
    manifest: &IconManifest,
) -> Option<(String, Option<u16>, Option<String>)> {
    let (provider, name) = id.split_once(':')?;
    let schema = manifest.provider(provider)?;

    let segments: Vec<&str> = name.split('-').collect();

    // Suffix-style first (`home-outline`, most providers) - longest
    // match wins, so a compound entry like `outline-rounded` in the
    // schema is preferred over a shorter `outline` that also happens to
    // match. Falls back to prefix-style (`outline-home` - Material
    // Icons' `ic` provider names it this way, `baseline`/`outline`/
    // `round`/`sharp`/`twotone` all leading, never trailing) only if no
    // suffix matched at all - a provider that's genuinely suffix-style
    // has no valid reason for its variant vocabulary to also appear as a
    // leading segment of an unrelated icon name, so trying prefix
    // unconditionally would risk misparsing e.g. an icon whose name
    // itself starts with a word that happens to be in another
    // provider's variant vocabulary copied in by `.override`.
    let mut variant: Option<String> = None;
    let mut family_segments: Vec<&str> = segments.clone();
    for suffix_len in (1..=segments.len()).rev() {
        let candidate = segments[segments.len() - suffix_len..].join("-");
        if schema.variants.iter().any(|known| *known == candidate) {
            variant = Some(candidate);
            family_segments = segments[..segments.len() - suffix_len].to_vec();
            break;
        }
    }
    if variant.is_none() {
        for prefix_len in (1..segments.len()).rev() {
            let candidate = segments[..prefix_len].join("-");
            if schema.variants.iter().any(|known| *known == candidate) {
                variant = Some(candidate);
                family_segments = segments[prefix_len..].to_vec();
                break;
            }
        }
    }

    let mut size: Option<u16> = None;
    if let Some(last) = family_segments.last() {
        if let Ok(parsed) = last.parse::<u16>() {
            if schema.sizes.contains(&parsed) {
                size = Some(parsed);
                family_segments.pop();
            }
        }
    }

    if family_segments.is_empty() {
        return None;
    }

    Some((family_segments.join("-"), size, variant))
}

fn default_iconify_id(
    family: &str,
    variant: Option<&str>,
    size: Option<u16>,
    defaults: &ManifestDefaults,
) -> Option<String> {
    let provider = defaults.provider.as_ref()?;
    let mut name = family.to_string();
    if let Some(size) = size {
        name.push('-');
        name.push_str(&size.to_string());
    }
    if let Some(variant) = variant {
        name.push('-');
        name.push_str(variant);
    }
    Some(format!("{provider}:{name}"))
}

/// Parses a `[glyph]` entry's `font-family:codepoint` spec, where
/// `codepoint` is either a single literal character or a `U+XXXX` hex escape.
/// Non-panicking - used at load/validation time (this module's `parse_entry`)
/// where a malformed spec should be a `ManifestError`, not a crash. See
/// [`parse_glyph_spec`] for the panicking wrapper macro-expansion/codegen
/// call sites use, where the manifest is assumed already validated.
pub fn try_parse_glyph_spec(spec: &str) -> Result<(String, char), String> {
    let Some((font_family, codepoint)) = spec.split_once(':') else {
        return Err(format!("must use `font-family:codepoint`, got `{spec}`"));
    };
    let font_family = font_family.trim();
    let codepoint = codepoint.trim();
    let ch = if codepoint.chars().count() == 1 {
        codepoint.chars().next().unwrap()
    } else {
        let normalized = codepoint
            .strip_prefix("U+")
            .or_else(|| codepoint.strip_prefix("u+"))
            .unwrap_or(codepoint);
        let scalar = u32::from_str_radix(normalized, 16)
            .map_err(|_| format!("has invalid codepoint `{codepoint}`"))?;
        char::from_u32(scalar).ok_or_else(|| format!("has non-scalar codepoint `{codepoint}`"))?
    };
    Ok((font_family.to_string(), ch))
}

/// Parses a `[glyph]` entry's `font-family:codepoint` spec, panicking with
/// `context` (the entry's key) on a malformed spec - for codegen/macro call
/// sites (`guicons-macros`, `guicons-build`) that only ever run against a
/// manifest `icons check`/`load_icon_manifest` has already validated via
/// [`try_parse_glyph_spec`], so a failure here means the cached/materialized
/// data itself is corrupt, not a user input problem worth a graceful error.
pub fn parse_glyph_spec(spec: &str, context: &str) -> (String, char) {
    try_parse_glyph_spec(spec).unwrap_or_else(|message| panic!("Glyph manifest entry `{context}` {message}"))
}

pub(crate) fn resolve_file_from_roots(roots: &[PathBuf], value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    roots
        .iter()
        .map(|root| root.join(path))
        .find(|candidate| candidate.exists())
        .unwrap_or_else(|| roots.first().unwrap_or(&PathBuf::from(".")).join(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::Diagnostics;

    fn workspace_root() -> PathBuf {
        PathBuf::from("/workspace")
    }

    fn with_root() -> ManifestDefaults {
        ManifestDefaults {
            roots: vec![workspace_root()],
            ..ManifestDefaults::default()
        }
    }

    fn entries_for(toml: &str, defaults: ManifestDefaults) -> (Vec<IconEntry>, Vec<String>) {
        let mut root = toml_span::parse(toml).expect("valid toml");
        let table = take_table(&mut root).expect("root must be a table");
        let mut errors = Vec::new();
        let mut entries = Vec::new();
        // The builtin provider set, same as what `load.rs` always merges
        // in for its own file (see `resolve_providers`) before entries are
        // collected - needed so auto-built iconify ids (via
        // `defaults.provider`) can be validated against a real schema.
        let providers = {
            let mut diags = Diagnostics {
                file: Path::new("test.gui.toml"),
                errors: &mut errors,
            };
            resolve_providers(HashMap::new(), &mut diags)
        };
        {
            let mut diags = Diagnostics {
                file: Path::new("test.gui.toml"),
                errors: &mut errors,
            };
            collect_entries(Vec::new(), table, &workspace_root(), &defaults, &providers, None, &mut diags, &mut entries);
        }
        (entries, errors.into_iter().map(|e| e.message).collect())
    }

    /// `insta::assert_debug_snapshot!`, but normalizing a `\`-separated
    /// Windows path to `/` first - `PathBuf`'s `Debug` output is
    /// inherently platform-dependent (`resolve_file_from_roots`/
    /// `resolve_workspace_path` join with the host OS's separator), so a
    /// snapshot recorded on Windows and one recorded on Linux would
    /// otherwise never agree even when the paths are logically identical.
    /// A macro, not a function - insta names a snapshot after its
    /// *immediate* calling function (via `#[track_caller]`), so a `fn`
    /// wrapper would make every call site share one made-up
    /// `assert_snapshot_normalized-N` name instead of each test's own.
    /// Only used by the handful of tests below whose `entries`/`defaults`
    /// actually embed a path - every other `assert_debug_snapshot!` call
    /// in this module snapshots something path-free and is untouched.
    macro_rules! assert_snapshot_normalized {
        ($value:expr) => {
            insta::with_settings!({filters => vec![(r"\\+", "/")]}, {
                insta::assert_debug_snapshot!($value);
            });
        };
    }

    fn defaults_for(toml: &str) -> (ManifestDefaults, Vec<String>) {
        let mut root = toml_span::parse(toml).expect("valid toml");
        let mut table = take_table(&mut root).expect("root must be a table");
        let defaults_value = table.remove("defaults");
        let mut errors = Vec::new();
        let defaults = {
            let mut diags = Diagnostics {
                file: Path::new("test.gui.toml"),
                errors: &mut errors,
            };
            parse_defaults(defaults_value, &workspace_root(), &workspace_root(), None, &mut diags)
        };
        (defaults, errors.into_iter().map(|e| e.message).collect())
    }

    #[test]
    fn defaults_provider_leaves_explicit_non_iconify_sources_alone() {
        let (defaults, errors) = defaults_for("[defaults]\nprovider = \"fluent\"\nsize = 24\n");
        assert!(errors.is_empty(), "{errors:?}");
        let defaults = ManifestDefaults { roots: vec![workspace_root()], ..defaults };
        let (entries, errors) = entries_for(
            r#"
            [logo]
            file = "logo.svg"

            [remote]
            url = "https://example.com/remote.svg"

            [spinner]
            glyph = "Segoe Fluent Icons:U+E001"
            "#,
            defaults,
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn size_groups_next_to_variants_are_kept() {
        let (entries, errors) = entries_for(
            r#"
            [settings]
            variants.filled = { file = "settings-filled.svg" }

            [settings.24]
            variants.filled = { file = "settings-24-filled.svg" }
            "#,
            with_root(),
        );
        assert!(errors.is_empty(), "{errors:?}");
        let keys: Vec<&str> = entries.iter().map(IconEntry::key).collect();
        assert_eq!(keys, ["settings-filled", "settings-24-filled"]);
    }

    #[test]
    fn stray_keys_next_to_variants_are_errors() {
        let (_, errors) = entries_for(
            r#"
            [settings]
            dynamic = true
            variants.filled = { file = "settings-filled.svg" }
            "#,
            with_root(),
        );
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("dynamic"), "{errors:?}");
    }

    #[test]
    fn simple_file_entry() {
        let (entries, errors) = entries_for(
            r#"
            [settings]
            file = "settings.svg"
            "#,
            with_root(),
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_snapshot_normalized!(entries);
    }

    #[test]
    fn variants_produce_two_entries_sharing_a_family() {
        let (entries, errors) = entries_for(
            r#"
            [settings]
            variants.filled = { file = "settings-filled.svg" }
            variants.regular = { file = "settings-regular.svg" }
            "#,
            with_root(),
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_snapshot_normalized!(entries);
    }

    #[test]
    fn nested_groups_build_a_dashed_key() {
        let (entries, errors) = entries_for(
            r#"
            [icons.navigation.back]
            file = "back.svg"
            "#,
            with_root(),
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_snapshot_normalized!(entries);
    }

    #[test]
    fn unsupported_field_is_rejected() {
        let (entries, errors) = entries_for(
            r#"
            [settings]
            file = "settings.svg"
            bogus = "oops"
            "#,
            with_root(),
        );
        assert!(entries.is_empty());
        insta::assert_debug_snapshot!(errors);
    }

    #[test]
    fn two_sources_is_an_error() {
        let (entries, errors) = entries_for(
            r#"
            [settings]
            file = "settings.svg"
            url = "https://example.com/a.svg"
            "#,
            with_root(),
        );
        assert!(entries.is_empty());
        insta::assert_debug_snapshot!(errors);
    }

    #[test]
    fn no_source_is_an_error() {
        let (entries, errors) = entries_for(
            r#"
            [settings]
            dynamic = true
            "#,
            with_root(),
        );
        assert!(entries.is_empty());
        insta::assert_debug_snapshot!(errors);
    }

    #[test]
    fn wrong_field_type_is_rejected() {
        let (entries, errors) = entries_for(
            r#"
            [settings]
            file = "settings.svg"
            dynamic = "yes"
            "#,
            with_root(),
        );
        assert!(entries.is_empty());
        insta::assert_debug_snapshot!(errors);
    }

    #[test]
    fn variants_must_be_a_table() {
        let (entries, errors) = entries_for(
            r#"
            [settings]
            variants = "nope"
            "#,
            with_root(),
        );
        assert!(entries.is_empty());
        insta::assert_debug_snapshot!(errors);
    }

    #[test]
    fn variant_entry_must_be_an_inline_table() {
        let (entries, errors) = entries_for(
            r#"
            [settings]
            variants.filled = "settings-filled.svg"
            "#,
            with_root(),
        );
        assert!(entries.is_empty());
        insta::assert_debug_snapshot!(errors);
    }

    #[test]
    fn default_provider_fills_in_iconify_id() {
        let defaults = ManifestDefaults {
            provider: Some("fluent".to_string()),
            size: Some(24),
            ..ManifestDefaults::default()
        };
        let (entries, errors) = entries_for(
            r#"
            [settings]
            variants.filled = {}
            "#,
            defaults,
        );
        assert!(errors.is_empty(), "{errors:?}");
        insta::assert_debug_snapshot!(entries);
    }

    #[test]
    fn explicit_iconify_overrides_default_provider() {
        let defaults = ManifestDefaults {
            provider: Some("fluent".to_string()),
            ..ManifestDefaults::default()
        };
        let (entries, errors) = entries_for(
            r#"
            [settings]
            iconify = "phosphor:gear"
            "#,
            defaults,
        );
        assert!(errors.is_empty(), "{errors:?}");
        insta::assert_debug_snapshot!(entries);
    }

    #[test]
    fn defaults_merge_root_and_roots() {
        let (defaults, errors) = defaults_for(
            r#"
            [defaults]
            root = "a"
            roots = ["b", "c"]
            "#,
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_snapshot_normalized!(defaults);
    }

    #[test]
    fn defaults_fall_back_to_manifest_and_workspace_dirs_when_empty() {
        let (defaults, errors) = defaults_for("");
        assert!(errors.is_empty(), "{errors:?}");
        insta::assert_debug_snapshot!(defaults);
    }

    #[test]
    fn defaults_reject_unsupported_field() {
        let (_, errors) = defaults_for(
            r#"
            [defaults]
            bogus = 1
            "#,
        );
        insta::assert_debug_snapshot!(errors);
    }

    #[test]
    fn numeric_path_segment_becomes_size_not_family() {
        let (entries, errors) = entries_for(
            r#"
            [settings.20]
            variants.filled = { file = "settings-20-filled.svg" }

            [settings.24]
            variants.filled = { file = "settings-24-filled.svg" }
            "#,
            with_root(),
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_snapshot_normalized!(entries);
    }

    #[test]
    fn numeric_path_segment_becomes_size_for_flat_entry() {
        let (entries, errors) = entries_for(
            r#"
            [settings.20]
            file = "settings-20.svg"
            "#,
            with_root(),
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_snapshot_normalized!(entries);
    }

    #[test]
    fn explicit_size_feeds_default_iconify_id() {
        let defaults = ManifestDefaults {
            provider: Some("fluent".to_string()),
            ..ManifestDefaults::default()
        };
        let (entries, errors) = entries_for(
            r#"
            [settings.20]
            variants.filled = {}
            "#,
            defaults,
        );
        assert!(errors.is_empty(), "{errors:?}");
        insta::assert_debug_snapshot!(entries);
    }

    #[test]
    fn defaults_reject_wrong_field_type() {
        let (_, errors) = defaults_for(
            r#"
            [defaults]
            size = "big"
            "#,
        );
        insta::assert_debug_snapshot!(errors);
    }

    fn paints_for(toml: &str) -> (Vec<(String, Option<String>)>, Vec<String>) {
        let (manifest, errors) =
            crate::load_icon_manifest_from_str(Path::new("/workspace/icons.gui.toml"), toml);
        let paints = manifest
            .entries()
            .iter()
            .map(|entry| {
                let paint = entry.paint().map(|paint| match paint.light == paint.dark {
                    true => paint.light.to_hex(),
                    false => format!("{}/{}", paint.light.to_hex(), paint.dark.to_hex()),
                });
                (entry.key().to_string(), paint)
            })
            .collect();
        (paints, errors.into_iter().map(|e| e.message).collect())
    }

    fn paint_of<'a>(paints: &'a [(String, Option<String>)], key: &str) -> Option<&'a str> {
        paints
            .iter()
            .find(|(entry, _)| entry == key)
            .unwrap_or_else(|| panic!("no entry `{key}` in {paints:?}"))
            .1
            .as_deref()
    }

    #[test]
    fn paint_resolves_nearest_declaration_first() {
        let (paints, errors) = paints_for(
            r##"
            [defaults]
            paint = "#111111"

            [providers.acme]
            paint = "#222222"

            [providers.tabler.override]
            paint = "#333333"

            [plain]
            iconify = "mdi:home"

            [from-provider]
            iconify = "acme:gear"

            [from-builtin-override]
            iconify = "tabler:clock"

            [group]
            paint = "#444444"

            [group.inner]
            iconify = "acme:gear"

            [family]
            paint = "#555555"
            variants.a = { iconify = "acme:a" }
            variants.b = { iconify = "acme:b", paint = "#666" }
            "##,
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(paint_of(&paints, "plain"), Some("#111111"));
        assert_eq!(paint_of(&paints, "from-provider"), Some("#222222"));
        assert_eq!(paint_of(&paints, "from-builtin-override"), Some("#333333"));
        assert_eq!(paint_of(&paints, "group-inner"), Some("#444444"));
        assert_eq!(paint_of(&paints, "family-a"), Some("#555555"));
        assert_eq!(paint_of(&paints, "family-b"), Some("#666666"));
    }

    #[test]
    fn paint_can_differ_per_theme() {
        let (paints, errors) = paints_for(
            r##"
            [defaults]
            paint = { light = "#1a1a1a", dark = "#fff" }

            [providers.acme]
            paint.light = "#111"
            paint.dark = "#222"

            [home]
            iconify = "mdi:home"

            [gear]
            iconify = "acme:gear"

            [prohibited]
            iconify = "mdi:cancel"
            paint = "#c42b1c"
            "##,
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(paint_of(&paints, "home"), Some("#1a1a1a/#ffffff"));
        assert_eq!(paint_of(&paints, "gear"), Some("#111111/#222222"));
        assert_eq!(paint_of(&paints, "prohibited"), Some("#c42b1c"));
    }

    #[test]
    fn malformed_theme_paint_is_an_error() {
        let (_, errors) = paints_for(
            r##"
            [only-light]
            iconify = "mdi:home"
            paint = { light = "#fff" }

            [unknown-theme]
            iconify = "mdi:home"
            paint = { light = "#fff", dark = "#000", sepia = "#700" }

            [not-a-color]
            iconify = "mdi:home"
            paint = 3
            "##,
        );
        insta::assert_debug_snapshot!(errors);
    }

    #[test]
    fn paint_none_cancels_what_comes_from_above() {
        let (paints, errors) = paints_for(
            r##"
            [defaults]
            paint = "#111111"

            [providers.logos]
            paint = "none"

            [ubuntu]
            iconify = "logos:ubuntu"

            [docker]
            iconify = "mdi:docker"
            paint = "none"
            "##,
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(paint_of(&paints, "ubuntu"), None);
        assert_eq!(paint_of(&paints, "docker"), None);
    }

    #[test]
    fn nothing_declared_means_no_paint() {
        let (paints, errors) = paints_for(
            r#"
            [home]
            iconify = "mdi:home"
            "#,
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(paint_of(&paints, "home"), None);
    }

    #[test]
    fn inherited_paint_skips_what_cant_be_painted() {
        let (paints, errors) = paints_for(
            r##"
            [defaults]
            paint = "#111111"

            [photo]
            file = "photo.png"

            [spinner]
            glyph = "Icons:U+E001"
            "##,
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(paint_of(&paints, "photo"), None);
        assert_eq!(paint_of(&paints, "spinner"), None);
    }

    #[test]
    fn a_file_that_is_neither_svg_nor_png_is_an_error() {
        let (entries, errors) = entries_for(
            r#"
            [photo]
            file = "photo.jpg"

            [shout]
            file = "SHOUT.SVG"
            "#,
            with_root(),
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("neither SVG nor PNG"), "{errors:?}");
    }

    #[test]
    fn explicit_paint_on_a_glyph_is_an_error() {
        let (paints, errors) = paints_for(
            r##"
            [spinner]
            glyph = "Icons:U+E001"
            paint = "#111111"
            "##,
        );
        assert!(paints.is_empty());
        insta::assert_debug_snapshot!(errors);
    }

    #[test]
    fn malformed_paint_is_an_error() {
        let (_, errors) = paints_for(
            r##"
            [defaults]
            paint = "red"

            [home]
            iconify = "mdi:home"
            paint = "#12345"
            "##,
        );
        insta::assert_debug_snapshot!(errors);
    }

    #[test]
    fn paint_color_parses_short_and_long_hex() {
        use crate::{Paint, PaintColor};
        assert_eq!(Paint::parse("none"), Ok(Paint::None));
        assert_eq!(PaintColor::parse("#fA0").map(PaintColor::to_hex), Ok("#ffaa00".to_string()));
        assert_eq!(PaintColor::parse("#1a2B3c").map(PaintColor::to_hex), Ok("#1a2b3c".to_string()));
        for bad in ["", "#", "fff", "#ffff", "#ggg", "#+1+2+3"] {
            assert!(PaintColor::parse(bad).is_err(), "`{bad}` should be rejected");
        }
    }

    #[test]
    fn current_color_is_found_in_any_case() {
        assert!(crate::svg_uses_current_color(br#"<path fill="currentColor"/>"#));
        assert!(crate::svg_uses_current_color(b"<path style=\"fill:currentcolor\"/>"));
        assert!(!crate::svg_uses_current_color(br##"<path fill="#fff"/>"##));
    }

    #[test]
    fn paint_svg_replaces_every_current_color() {
        let color = crate::PaintColor { r: 0x12, g: 0xab, b: 0xff };
        assert_eq!(
            crate::paint_svg(b"<path stroke=\"currentColor\" style=\"fill:currentcolor\"/>", color),
            b"<path stroke=\"#12abff\" style=\"fill:#12abff\"/>"
        );
    }

    fn providers_for(toml: &str) -> (HashMap<String, ProviderSchema>, Vec<String>) {
        let mut root = toml_span::parse(toml).expect("valid toml");
        let mut table = take_table(&mut root).expect("root must be a table");
        let providers_value = table.remove("providers");
        let mut errors = Vec::new();
        let providers = {
            let mut diags = Diagnostics {
                file: Path::new("test.gui.toml"),
                errors: &mut errors,
            };
            let declarations = parse_providers(providers_value, &mut diags);
            resolve_providers(declarations, &mut diags)
        };
        (providers, errors.into_iter().map(|e| e.message).collect())
    }

    fn schema(variants: &[&str], sizes: &[u16]) -> ProviderSchema {
        ProviderSchema {
            variants: variants.iter().map(|v| v.to_string()).collect(),
            sizes: sizes.to_vec(),
            paint: None,
        }
    }

    fn manifest_with_providers(providers: HashMap<String, ProviderSchema>) -> IconManifest {
        IconManifest {
            manifest_path: PathBuf::from("icons.gui.toml"),
            workspace_root: PathBuf::from("/workspace"),
            source_paths: Vec::new(),
            entries: Vec::new(),
            providers,
            default_paint: None,
            roots: Vec::new(),
        }
    }

    #[test]
    fn parses_a_custom_provider_schema() {
        let (providers, errors) = providers_for(
            r#"
            [providers.acme]
            variants = ["outline", "solid"]
            sizes = [16, 24]
            "#,
        );
        assert!(errors.is_empty(), "{errors:?}");
        let acme = providers.get("acme").expect("acme provider");
        assert_eq!(acme.variants, vec!["outline", "solid"]);
        assert_eq!(acme.sizes, vec![16, 24]);
    }

    #[test]
    fn provider_without_variants_or_sizes_defaults_to_empty() {
        let (providers, errors) = providers_for(
            r#"
            [providers.acme]
            "#,
        );
        assert!(errors.is_empty(), "{errors:?}");
        let acme = providers.get("acme").expect("acme provider");
        assert!(acme.variants.is_empty());
        assert!(acme.sizes.is_empty());
    }

    #[test]
    fn providers_entry_must_be_a_table() {
        let (_, errors) = providers_for(
            r#"
            providers = "nope"
            "#,
        );
        insta::assert_debug_snapshot!(errors);
    }

    #[test]
    fn provider_schema_rejects_wrong_field_type() {
        let (_, errors) = providers_for(
            r#"
            [providers.acme]
            sizes = "big"
            "#,
        );
        insta::assert_debug_snapshot!(errors);
    }

    #[test]
    fn builtin_providers_are_available_even_when_unmentioned() {
        let (providers, errors) = providers_for("");
        assert!(errors.is_empty(), "{errors:?}");
        let fluent = providers.get("fluent").expect("builtin fluent provider");
        assert_eq!(fluent.variants, vec!["regular", "filled"]);
        assert_eq!(fluent.sizes, vec![16, 20, 24, 28, 32, 48]);
        for name in ["ph", "material-symbols", "heroicons", "bi", "tabler"] {
            assert!(providers.contains_key(name), "missing builtin `{name}`");
        }
    }

    #[test]
    fn redefining_a_builtin_provider_without_override_is_an_error() {
        let (providers, errors) = providers_for(
            r#"
            [providers.fluent]
            variants = ["custom"]
            "#,
        );
        assert_eq!(
            providers.get("fluent").expect("builtin fluent provider").variants,
            vec!["regular", "filled"],
            "rejected redefinition must not replace the builtin"
        );
        insta::assert_debug_snapshot!(errors);
    }

    #[test]
    fn override_replaces_only_the_fields_it_specifies() {
        let (providers, errors) = providers_for(
            r#"
            [providers.fluent.override]
            variants = ["regular", "filled", "light"]
            "#,
        );
        assert!(errors.is_empty(), "{errors:?}");
        let fluent = providers.get("fluent").expect("fluent provider");
        assert_eq!(fluent.variants, vec!["regular", "filled", "light"]);
        assert_eq!(fluent.sizes, vec![16, 20, 24, 28, 32, 48]);
    }

    #[test]
    fn override_on_a_non_builtin_name_is_an_error() {
        let (_, errors) = providers_for(
            r#"
            [providers.acme.override]
            variants = ["solid"]
            "#,
        );
        insta::assert_debug_snapshot!(errors);
    }

    #[test]
    fn mixing_own_fields_with_override_is_an_error() {
        let (_, errors) = providers_for(
            r#"
            [providers.fluent]
            variants = ["custom"]
            override = { variants = ["light"] }
            "#,
        );
        insta::assert_debug_snapshot!(errors);
    }

    #[test]
    fn decompose_fluent_style_size_and_variant() {
        let mut providers = HashMap::new();
        providers.insert("fluent".to_string(), schema(&["regular", "filled"], &[16, 20, 24, 28, 32, 48]));
        let manifest = manifest_with_providers(providers);
        insta::assert_debug_snapshot!(decompose_iconify_id(
            "fluent:add-square-multiple-24-regular",
            &manifest
        ));
    }

    #[test]
    fn decompose_phosphor_style_bare_default_has_no_suffix() {
        let mut providers = HashMap::new();
        providers.insert("ph".to_string(), schema(&["thin", "light", "bold", "fill", "duotone"], &[]));
        let manifest = manifest_with_providers(providers);
        insta::assert_debug_snapshot!(decompose_iconify_id("ph:acorn", &manifest));
    }

    #[test]
    fn decompose_phosphor_style_with_variant_suffix() {
        let mut providers = HashMap::new();
        providers.insert("ph".to_string(), schema(&["thin", "light", "bold", "fill", "duotone"], &[]));
        let manifest = manifest_with_providers(providers);
        insta::assert_debug_snapshot!(decompose_iconify_id("ph:acorn-bold", &manifest));
    }

    #[test]
    fn decompose_material_symbols_style_compound_variant_matches_greedily() {
        let mut providers = HashMap::new();
        providers.insert(
            "material-symbols".to_string(),
            schema(&["outline", "rounded", "sharp", "outline-rounded", "outline-sharp"], &[]),
        );
        let manifest = manifest_with_providers(providers);
        insta::assert_debug_snapshot!(decompose_iconify_id(
            "material-symbols:3d-rotation-outline-rounded",
            &manifest
        ));
    }

    #[test]
    fn decompose_bootstrap_style_leading_digit_name_is_not_mistaken_for_size() {
        let mut providers = HashMap::new();
        providers.insert("bi".to_string(), schema(&["fill"], &[]));
        let manifest = manifest_with_providers(providers);
        insta::assert_debug_snapshot!(decompose_iconify_id("bi:0-circle", &manifest));
    }

    #[test]
    fn decompose_unknown_provider_returns_none() {
        let manifest = manifest_with_providers(HashMap::new());
        assert_eq!(decompose_iconify_id("unknown:whatever", &manifest), None);
    }

    #[test]
    fn decompose_id_without_colon_returns_none() {
        let manifest = manifest_with_providers(HashMap::new());
        assert_eq!(decompose_iconify_id("no-colon-here", &manifest), None);
    }

    mod decompose_proptests {
        use super::*;
        use proptest::prelude::*;

        fn schema_strategy() -> impl Strategy<Value = ProviderSchema> {
            (
                prop::collection::vec("[a-z]{2,6}", 0..4),
                prop::collection::vec(1u16..100, 0..4),
            )
                .prop_map(|(variants, sizes)| ProviderSchema { variants, sizes, paint: None })
        }

        proptest! {
            /// Building an id the same way `default_iconify_id` does, then
            /// decomposing it with the same schema, must recover the
            /// original family/size/variant - as long as the family itself
            /// doesn't happen to look like one of the schema's own suffixes
            /// (an inherent, expected ambiguity of suffix-stripping, not
            /// something to paper over here).
            #[test]
            fn round_trips_through_build_and_decompose(
                family_segments in prop::collection::vec("[a-z]{2,8}", 1..3),
                schema in schema_strategy(),
                use_variant in any::<bool>(),
                use_size in any::<bool>(),
            ) {
                prop_assume!(family_segments.iter().all(|segment| !schema.variants.contains(segment)));

                let family = family_segments.join("-");
                let variant = if use_variant { schema.variants.first().cloned() } else { None };
                let size = if use_size { schema.sizes.first().copied() } else { None };

                let mut name = family.clone();
                if let Some(size) = size {
                    name.push('-');
                    name.push_str(&size.to_string());
                }
                if let Some(variant) = &variant {
                    name.push('-');
                    name.push_str(variant);
                }
                let id = format!("provider:{name}");

                let mut providers = HashMap::new();
                providers.insert("provider".to_string(), schema);
                let manifest = manifest_with_providers(providers);

                prop_assert_eq!(decompose_iconify_id(&id, &manifest), Some((family, size, variant)));
            }

            /// No input string, however malformed, should panic - the point
            /// of a suffix-membership-based parser is that "no match" is
            /// just `None`/unchanged, never a crash.
            #[test]
            fn never_panics_on_arbitrary_input(id in ".*", schema in schema_strategy()) {
                let mut providers = HashMap::new();
                providers.insert("provider".to_string(), schema);
                let manifest = manifest_with_providers(providers);
                let _ = decompose_iconify_id(&id, &manifest);
            }
        }
    }
}

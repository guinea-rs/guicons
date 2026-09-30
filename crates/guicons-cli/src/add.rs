use guicons_core::IconManifest;
use std::fs;
use std::path::Path;
use toml_edit::{DocumentMut, InlineTable, Item, Table, Value};

#[derive(Debug)]
pub enum AddError {
    Manifest(Vec<String>),
    AlreadyExists(Vec<String>),
    Plan(String),
    Io(String),
    InvalidResult(Vec<String>),
}

struct AddPlan {
    family: String,
    size: Option<u16>,
    field_name: &'static str,
    /// (variant, field value) - `variant: None` means the field goes
    /// straight on the family/size table, not under `variants.<name>`.
    items: Vec<(Option<String>, String)>,
}

/// Writes a new entry into `icons.gui.toml`, preserving everything else in
/// the file (`toml_edit`, not `guicons-core`'s read-only `toml_span`
/// parser). Returns the manifest keys that were added.
pub fn add(
    manifest_path: &Path,
    source: &str,
    name: Option<&str>,
    variants: &[String],
    size: Option<u16>,
    force: bool,
) -> Result<Vec<String>, AddError> {
    let manifest_path = &guicons_core::resolve_manifest_redirect(manifest_path);
    let existing_manifest = if manifest_path.exists() {
        let (manifest, errors) = guicons_core::load_icon_manifest(manifest_path);
        if !errors.is_empty() {
            return Err(AddError::Manifest(errors.iter().map(|e| e.to_string()).collect()));
        }
        Some(manifest)
    } else {
        None
    };

    let plan = plan_add(manifest_path, source, name, variants, size, existing_manifest.as_ref()).map_err(AddError::Plan)?;

    if !force {
        if let Some(manifest) = &existing_manifest {
            let collisions: Vec<String> = plan
                .items
                .iter()
                .map(|(variant, _)| compute_key(&plan.family, plan.size, variant.as_deref()))
                .filter(|key| manifest.entry_for_key(key).is_some())
                .collect();
            if !collisions.is_empty() {
                return Err(AddError::AlreadyExists(collisions));
            }
        }
    }

    let existing_content = if manifest_path.exists() {
        fs::read_to_string(manifest_path).map_err(|e| AddError::Io(e.to_string()))?
    } else {
        String::new()
    };
    let mut doc = existing_content
        .parse::<DocumentMut>()
        .map_err(|e| AddError::Io(e.to_string()))?;

    let path = table_path(&plan.family, plan.size);
    for (variant, value) in &plan.items {
        set_entry(&mut doc, &path, variant.as_deref(), plan.field_name, value);
    }
    let new_content = doc.to_string();

    validate(manifest_path, &new_content)?;

    guicons_net::write_atomic(manifest_path, new_content.as_bytes()).map_err(|e| AddError::Io(e.to_string()))?;

    Ok(plan
        .items
        .iter()
        .map(|(variant, _)| compute_key(&plan.family, plan.size, variant.as_deref()))
        .collect())
}

/// Writes `content` to a scratch file next to the real manifest and
/// re-parses it with `guicons-core` before the real file is touched -
/// `toml_edit` only guarantees the result is syntactically valid TOML, not
/// that it's still a valid guicons manifest.
fn validate(manifest_path: &Path, content: &str) -> Result<(), AddError> {
    let dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let scratch = tempfile::Builder::new()
        .prefix(".icons-add-")
        .suffix(".gui.toml")
        .tempfile_in(dir)
        .map_err(|e| AddError::Io(e.to_string()))?;
    fs::write(scratch.path(), content).map_err(|e| AddError::Io(e.to_string()))?;
    let (_, errors) = guicons_core::load_icon_manifest(scratch.path());
    if errors.is_empty() {
        Ok(())
    } else {
        Err(AddError::InvalidResult(errors.iter().map(|e| e.to_string()).collect()))
    }
}

fn plan_add(
    manifest_path: &Path,
    source: &str,
    name: Option<&str>,
    variants: &[String],
    size: Option<u16>,
    manifest: Option<&IconManifest>,
) -> Result<AddPlan, String> {
    let iconify = source
        .split_once(':')
        .filter(|(provider, _)| is_iconify_prefix(provider) && !Path::new(source).exists());
    if let Some((provider, base)) = iconify {
        if base.is_empty() {
            return Err(format!("`{source}` has no icon name after the `:`"));
        }

        if !variants.is_empty() {
            let family = name
                .ok_or("`--name` is required when using `--variants`")?
                .to_string();
            let items = variants
                .iter()
                .map(|variant| {
                    let mut id = format!("{provider}:{base}");
                    if let Some(size) = size {
                        id.push('-');
                        id.push_str(&size.to_string());
                    }
                    id.push('-');
                    id.push_str(variant);
                    (Some(variant.clone()), id)
                })
                .collect();
            return Ok(AddPlan { family, size, field_name: "iconify", items });
        }

        if let Some(manifest) = manifest {
            if let Some((family, decomposed_size, variant)) = guicons_core::decompose_iconify_id(source, manifest) {
                return Ok(AddPlan {
                    family: name.map(str::to_string).unwrap_or(family),
                    size: decomposed_size,
                    field_name: "iconify",
                    items: vec![(variant, source.to_string())],
                });
            }
        }

        let family = name.map(str::to_string).unwrap_or_else(|| base.to_string());
        return Ok(AddPlan {
            family,
            size: None,
            field_name: "iconify",
            items: vec![(None, source.to_string())],
        });
    }

    if !variants.is_empty() {
        return Err("`--variants` only makes sense with an iconify source (`set:name`)".to_string());
    }
    let family = match name {
        Some(name) => name.to_string(),
        None => Path::new(source)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string)
            .ok_or_else(|| format!("could not derive a name from `{source}`; pass `--name`"))?,
    };
    Ok(AddPlan {
        family,
        size: None,
        field_name: "file",
        items: vec![(None, file_relative_to_roots(manifest_path, source, manifest)?)],
    })
}

fn is_iconify_prefix(provider: &str) -> bool {
    !provider.is_empty() && provider.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// `source` (relative to the current directory) as the manifest spells it:
/// relative to the first of its roots that contains it.
fn file_relative_to_roots(manifest_path: &Path, source: &str, manifest: Option<&IconManifest>) -> Result<String, String> {
    let file = guicons_core::canonicalize_or_self(Path::new(source));
    if !file.is_file() {
        return Err(format!("`{source}` doesn't exist"));
    }
    let manifest_dir = manifest_path.parent().filter(|dir| !dir.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let roots = match manifest {
        Some(manifest) if !manifest.roots().is_empty() => manifest.roots().to_vec(),
        _ => vec![manifest_dir.to_path_buf()],
    };
    roots
        .iter()
        .find_map(|root| file.strip_prefix(guicons_core::canonicalize_or_self(root)).ok())
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .ok_or_else(|| {
            let roots: Vec<String> = roots.iter().map(|root| root.display().to_string()).collect();
            format!("`{source}` isn't under the manifest's roots ({})", roots.join(", "))
        })
}

fn compute_key(family: &str, size: Option<u16>, variant: Option<&str>) -> String {
    let mut key = family.to_string();
    if let Some(size) = size {
        key.push('-');
        key.push_str(&size.to_string());
    }
    if let Some(variant) = variant {
        key.push('-');
        key.push_str(variant);
    }
    key
}

fn table_path(family: &str, size: Option<u16>) -> Vec<String> {
    let mut path = vec![family.to_string()];
    if let Some(size) = size {
        path.push(size.to_string());
    }
    path
}

/// The table at `parent[key]`, converting an inline table in place (keeping
/// its entries) and creating it if missing.
fn child_table<'a>(parent: &'a mut Table, key: &str) -> &'a mut Table {
    let item = parent.entry(key).or_insert_with(|| Item::Table(Table::new()));
    if let Some(inline) = item.as_inline_table().cloned() {
        *item = Item::Table(inline.into_table());
    } else if !item.is_table() {
        *item = Item::Table(Table::new());
    }
    item.as_table_mut().unwrap()
}

fn navigate_or_create<'a>(table: &'a mut Table, path: &[String]) -> &'a mut Table {
    let mut current = table;
    for segment in path {
        current = child_table(current, segment);
    }
    current
}

fn set_entry(doc: &mut DocumentMut, path: &[String], variant: Option<&str>, field_name: &str, field_value: &str) {
    let table = navigate_or_create(doc.as_table_mut(), path);
    match variant {
        Some(variant) => {
            let variants_table = child_table(table, "variants");
            let mut inline = InlineTable::new();
            inline.insert(field_name, Value::from(field_value));
            variants_table.insert(variant, Item::Value(Value::InlineTable(inline)));
        }
        None => {
            table.insert(field_name, toml_edit::value(field_value));
        }
    }
}

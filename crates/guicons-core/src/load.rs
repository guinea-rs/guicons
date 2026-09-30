//! Entry point for loading a manifest file (and any files it `[link]`ed manifests)
//! from disk.
//!
//! Loading is two phases: [`crate::graph`] discovers the `[link].includes`
//! file tree (an artifact - [`crate::graph::ManifestGraph`]) without
//! interpreting any of it, then this module "compiles" that artifact -
//! walking it bottom-up, turning each node's raw text into entries/
//! providers via [`crate::parse`] and merging children into parents.
//! Neither phase touches the filesystem itself except `graph`'s discovery
//! step.

use crate::diagnostics::{Diagnostics, ManifestError};
use crate::graph::{build_manifest_graph, ManifestFile, ManifestGraph};
use crate::model::{IconEntry, IconManifest};
use crate::parse::{collect_entries, parse_defaults, parse_providers, resolve_providers};
use crate::paths::{find_workspace_root, resolve_manifest_redirect, resolve_manifest_redirect_content};
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use std::collections::HashMap;
use std::path::Path;
use toml_span::value::ValueInner;

/// Parse a manifest (and any files it `[link]`ed manifests), collecting every
/// problem found instead of stopping at the first one.
///
/// Always returns a best-effort [`IconManifest`]: entries that failed to
/// parse are simply omitted, everything else is kept. Check whether the
/// returned error list is empty to know if the manifest is fully valid.
pub fn load_icon_manifest(manifest_path: &Path) -> (IconManifest, Vec<ManifestError>) {
    load(manifest_path, None)
}

/// Like [`load_icon_manifest`], but parses `content` for the root document
/// instead of reading `manifest_path` from disk. Any `[link]`d files are
/// still read from disk.
pub fn load_icon_manifest_from_str(manifest_path: &Path, content: &str) -> (IconManifest, Vec<ManifestError>) {
    load(manifest_path, Some(content))
}

fn load(manifest_path: &Path, content_override: Option<&str>) -> (IconManifest, Vec<ManifestError>) {
    // `manifest_path` itself might be a pointer (a stub `icons.gui.toml`
    // next to a crate that shares a manifest living elsewhere - see
    // `resolve_manifest_redirect`'s doc comment) - resolved before
    // anything else touches it, so every caller transparently ends up
    // loading the real manifest instead. `content_override` only still
    // applies once resolution lands back on the same path (i.e. it wasn't
    // a pointer).
    let resolved_path = match content_override {
        Some(content) => resolve_manifest_redirect_content(manifest_path, content),
        None => resolve_manifest_redirect(manifest_path),
    };
    let content_override = if resolved_path == manifest_path { content_override } else { None };
    let manifest_path = &resolved_path;

    let mut errors = Vec::new();
    let file_graph = build_manifest_graph(manifest_path, content_override, &mut errors);
    let source_paths: Vec<_> = file_graph.graph.node_weights().map(|file| file.path.clone()).collect();
    let manifest = compile(&file_graph, file_graph.root, &source_paths, &mut errors);
    check_duplicate_keys(&manifest.entries, &mut errors);
    (manifest, errors)
}

/// Two entries sharing the same `key()` - including one from the root
/// manifest colliding with one pulled in via `[link]` - is never checked
/// anywhere else: entries are just concatenated and sorted by key. Left
/// unnoticed, `IconManifest::entry_for_key` would silently return
/// whichever one `.find()` hits first (sort-order-dependent), so this runs
/// once, over the fully merged entry list, at every public load entry
/// point - not per-file inside `compile`'s recursive walk, which would
/// double-report collisions that already exist purely within one included
/// file.
fn check_duplicate_keys(entries: &[IconEntry], errors: &mut Vec<ManifestError>) {
    let mut first_seen: HashMap<&str, &IconEntry> = HashMap::new();
    for entry in entries {
        match first_seen.get(entry.key()) {
            Some(first) => errors.push(ManifestError {
                file: entry.file().to_path_buf(),
                span: Some(entry.span()),
                message: format!(
                    "duplicate icon key `{}` - already defined in {}",
                    entry.key(),
                    first.file().display()
                ),
            }),
            None => {
                first_seen.insert(entry.key(), entry);
            }
        }
    }
}

/// Convenience wrapper for `build.rs`: parse the manifest and panic with
/// every collected error if it isn't fully valid.
pub fn load_icon_manifest_or_panic(manifest_path: &Path) -> IconManifest {
    let (manifest, errors) = load_icon_manifest(manifest_path);
    if !errors.is_empty() {
        let mut message = format!(
            "failed to load icon manifest ({} error{}):\n",
            errors.len(),
            if errors.len() == 1 { "" } else { "s" }
        );
        for error in &errors {
            message.push_str("  - ");
            message.push_str(&error.to_string());
            message.push('\n');
        }
        panic!("{message}");
    }
    manifest
}

fn empty_manifest(manifest_path: &Path, source_paths: &[std::path::PathBuf]) -> IconManifest {
    IconManifest {
        manifest_path: manifest_path.to_path_buf(),
        workspace_root: manifest_path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf(),
        source_paths: source_paths.to_vec(),
        entries: Vec::new(),
        providers: std::collections::HashMap::new(),
        default_paint: None,
    }
}

/// Walks `file_graph` bottom-up from `node`, turning each file's raw text
/// into entries/providers and merging its children's (already-compiled)
/// entries/providers in first, matching how a file's *own* declarations
/// were always meant to win over anything pulled in via `[link]`. Children
/// are visited in `includes = [...]`'s declaration order (the edge
/// weight), not `petgraph`'s own adjacency order.
fn compile(
    file_graph: &ManifestGraph,
    node: NodeIndex,
    source_paths: &[std::path::PathBuf],
    errors: &mut Vec<ManifestError>,
) -> IconManifest {
    let ManifestFile { path: manifest_path, content } = &file_graph.graph[node];

    if content.is_empty() {
        // Discovery already recorded why (unreadable file, or this node is
        // itself a cycle's dead end) - nothing left to compile.
        return empty_manifest(manifest_path, source_paths);
    }

    let mut root_table = match toml_span::parse(content).expect("re-parsing already-validated content").take() {
        ValueInner::Table(table) => table,
        _ => unreachable!("discovery already rejected a non-table root"),
    };

    let manifest_dir = manifest_path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
    let workspace_root = find_workspace_root(manifest_path).unwrap_or_else(|| manifest_dir.clone());

    let defaults_value = root_table.remove("defaults");
    let defaults = {
        let mut diags = Diagnostics { file: manifest_path, errors };
        parse_defaults(defaults_value, &workspace_root, &manifest_dir, &mut diags)
    };

    let providers_value = root_table.remove("providers");
    let own_providers = {
        let mut diags = Diagnostics { file: manifest_path, errors };
        let declarations = parse_providers(providers_value, &mut diags);
        resolve_providers(declarations, &mut diags)
    };

    // Already validated (and its includes turned into graph edges) during
    // discovery - nothing left to do with it here except discard it so it
    // isn't mistaken for an icon entry below.
    root_table.remove("link");

    let mut children: Vec<_> = file_graph
        .graph
        .edges(node)
        .map(|edge| (*edge.weight(), edge.target()))
        .collect();
    children.sort_by_key(|(ordinal, _)| *ordinal);

    let mut entries = Vec::new();
    let mut providers = HashMap::new();
    for (_, child) in children {
        let child_manifest = compile(file_graph, child, source_paths, errors);
        entries.extend(child_manifest.entries);
        providers.extend(child_manifest.providers);
    }
    // A file's own `[providers.*]` take precedence over anything an
    // `[link]`d file declared under the same name.
    providers.extend(own_providers);

    let own_entries_start = entries.len();
    {
        let mut diags = Diagnostics { file: manifest_path, errors };
        collect_entries(Vec::new(), root_table, &workspace_root, &defaults, &providers, None, &mut diags, &mut entries);
    }
    // `collect_entries` doesn't know which file it's parsing (that's the
    // whole point of the parse/load split) - entries from `[link]`d
    // files already have `file` set correctly by their own recursive
    // `compile` call, so only stamp the ones this level just added for
    // its own root table.
    for entry in &mut entries[own_entries_start..] {
        entry.file = manifest_path.clone();
    }

    entries.sort_by(|a, b| a.key().cmp(b.key()));

    IconManifest {
        manifest_path: manifest_path.clone(),
        workspace_root,
        source_paths: source_paths.to_vec(),
        entries,
        providers,
        default_paint: defaults.paint,
    }
}

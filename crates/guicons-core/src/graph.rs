//! Phase 1 of loading a manifest: **discover** the `[link].includes` file
//! graph, without doing any of the semantic work ([`crate::parse`]'s job)
//! that turns a file's contents into entries/providers. This is the only
//! place that walks the filesystem or decides which files exist and how
//! they relate - [`crate::load`] (phase 2, "compile") only ever walks the
//! [`ManifestGraph`] this module hands it, never touches `fs::` itself,
//! and never resolves an `includes = [...]` path on its own.
//!
//! Splitting it this way means the two phases can't accidentally end up
//! with different ideas of which files exist or how they nest - there is
//! exactly one place that answers that question, and it produces a single
//! artifact the compile phase treats as ground truth.

use crate::diagnostics::{Diagnostics, ManifestError};
use crate::paths::{canonicalize_or_self, resolve_entry_path};
use petgraph::graph::{DiGraph, NodeIndex};
use std::fs;
use std::path::{Path, PathBuf};
use toml_span::de_helpers::TableHelper;
use toml_span::value::ValueInner;
use toml_span::{Deserialize, DeserError, Value};

/// Wraps a `Deserialize` value to also capture its own span - `Vec<String>`
/// alone (what `TableHelper::optional` normally gives back) loses each
/// array element's individual location, which is exactly what's needed to
/// point a "this included file doesn't exist" diagnostic at the specific
/// `includes = [...]` string that named it, rather than at the whole
/// `[link]` table.
struct WithSpan<T> {
    value: T,
    span: toml_span::Span,
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for WithSpan<T> {
    fn deserialize(value: &mut Value<'de>) -> Result<Self, DeserError> {
        let span = value.span;
        T::deserialize(value).map(|value| WithSpan { value, span })
    }
}

/// One discovered file: its path and raw text, kept around so the compile
/// phase never has to touch disk again. Empty `content` marks a file that
/// failed to read or parse - the error for *why* was already recorded
/// during discovery, so the compile phase just treats it as an empty leaf.
pub(crate) struct ManifestFile {
    pub(crate) path: PathBuf,
    pub(crate) content: String,
}

/// The `[link].includes` tree rooted at the manifest originally requested.
/// An edge `parent -> child`, ordered by `includes = [...]`'s declaration
/// order (stored as the edge weight, since `petgraph::Graph`'s own edge
/// iteration order isn't declaration order), means `parent` named `child`.
/// Not deduplicated by path: two different files including the same third
/// file get two separate nodes/subtrees, matching how `[providers.*]`/
/// entries from each occurrence are independently merged rather than
/// shared - only an include that's its own transitive ancestor (a real
/// cycle) is refused.
pub(crate) struct ManifestGraph {
    pub(crate) graph: DiGraph<ManifestFile, u32>,
    pub(crate) root: NodeIndex,
}

/// `path.display()`, without Windows' `\\?\` verbatim-path prefix - accurate
/// but just noise in a diagnostic message meant for a human to read.
fn display_path(path: &Path) -> String {
    let rendered = path.display().to_string();
    rendered.strip_prefix(r"\\?\").unwrap_or(&rendered).to_string()
}

/// `\r\n` -> `\n` (and a bare `\r` -> `\n`, for old Mac-style line endings)
/// - so byte offsets [`toml_span`] hands back (`IconEntry::span()`) don't
/// depend on the checkout's line endings. `content_override` is never
/// touched here.
fn normalize_line_endings(content: &str) -> String {
    if !content.contains('\r') {
        return content.to_string();
    }
    content.replace("\r\n", "\n").replace('\r', "\n")
}

/// Builds the [`ManifestGraph`] rooted at `manifest_path` - reads every
/// file reachable through `[link].includes`, validating only what's
/// needed to know the graph's shape (recursive-include detection, `[link]`
/// itself being well-formed, each target existing). Defaults/providers/
/// entries aren't interpreted here at all; that's the compile phase's job
/// once it has this whole artifact in hand.
pub(crate) fn build_manifest_graph(
    manifest_path: &Path,
    content_override: Option<&str>,
    errors: &mut Vec<ManifestError>,
) -> ManifestGraph {
    let mut graph = DiGraph::new();
    let mut ancestors = Vec::new();
    let root = discover(&mut graph, &mut ancestors, manifest_path, content_override, errors)
        .unwrap_or_else(|| graph.add_node(ManifestFile { path: canonicalize_or_self(manifest_path), content: String::new() }));
    ManifestGraph { graph, root }
}

/// `None` only for a genuine cycle (this path is already an ancestor
/// currently being discovered) - the caller simply skips adding an edge
/// for that include, since the error was already recorded here.
fn discover(
    graph: &mut DiGraph<ManifestFile, u32>,
    ancestors: &mut Vec<PathBuf>,
    manifest_path: &Path,
    content_override: Option<&str>,
    errors: &mut Vec<ManifestError>,
) -> Option<NodeIndex> {
    let manifest_path = canonicalize_or_self(manifest_path);

    if ancestors.contains(&manifest_path) {
        errors.push(ManifestError {
            file: manifest_path.clone(),
            span: None,
            message: "recursive icon manifest include".to_string(),
        });
        return None;
    }
    ancestors.push(manifest_path.clone());

    let content = match content_override {
        Some(content) => content.to_string(),
        None => match fs::read_to_string(&manifest_path).map(|content| normalize_line_endings(&content)) {
            Ok(content) => content,
            Err(e) => {
                errors.push(ManifestError {
                    file: manifest_path.clone(),
                    span: None,
                    message: format!("failed to read file: {e}"),
                });
                ancestors.pop();
                return Some(graph.add_node(ManifestFile { path: manifest_path, content: String::new() }));
            }
        },
    };

    // A top-level TOML syntax error (or a non-table root) makes `content`
    // itself unusable - the compile phase re-parses from scratch and can't
    // do anything with it either, so store it empty (same "already failed,
    // nothing to compile" signal as an unreadable file) rather than a
    // string `compile` would only panic trying to re-parse.
    let Some(includes) = extract_includes(&manifest_path, &content, errors) else {
        ancestors.pop();
        return Some(graph.add_node(ManifestFile { path: manifest_path, content: String::new() }));
    };
    let node = graph.add_node(ManifestFile { path: manifest_path.clone(), content });

    let base = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    for (ordinal, include) in includes.into_iter().enumerate() {
        if !include.value.ends_with(".gui.toml") {
            errors.push(ManifestError {
                file: manifest_path.clone(),
                span: Some(include.span.into()),
                message: format!("`[link]` includes a file that isn't a `.gui.toml` manifest: `{}`", include.value),
            });
            continue;
        }
        let child_path = resolve_entry_path(base, &include.value);
        if !child_path.exists() {
            errors.push(ManifestError {
                file: manifest_path.clone(),
                span: Some(include.span.into()),
                message: format!(
                    "`[link]` includes a file that doesn't exist: `{}` (resolved to {})",
                    include.value,
                    display_path(&child_path)
                ),
            });
            continue;
        }
        if let Some(child) = discover(graph, ancestors, &child_path, None, errors) {
            graph.add_edge(node, child, ordinal as u32);
        }
    }

    ancestors.pop();
    Some(node)
}

/// Parses just enough of `content` to validate `[link]` and pull out its
/// `includes` list - not TOML syntax (already known good by construction:
/// this is the same parse the compile phase will redo from scratch) but
/// the same shape checks `TableHelper`/`Deserialize` would apply during
/// full semantic parsing, so a malformed `[link]` is reported exactly
/// once, here, rather than risking a second, duplicate report when the
/// compile phase later parses this same file's `defaults`/`providers`/
/// entries.
///
/// `None` only for a top-level failure (TOML syntax error, or a non-table
/// root) that makes the rest of `content` unusable too - the caller marks
/// the whole file as failed rather than treating it as merely missing an
/// `[link]`. A malformed `[link]` *table* itself (wrong field type, not a
/// table, unknown field) doesn't reach this branch: the rest of the file
/// can still be compiled normally, so this returns `Some(vec![])` and lets
/// the compile phase silently discard `[link]` as already-reported-bad.
fn extract_includes(manifest_path: &Path, content: &str, errors: &mut Vec<ManifestError>) -> Option<Vec<WithSpan<String>>> {
    let mut root_value = match toml_span::parse(content) {
        Ok(value) => value,
        Err(e) => {
            errors.push(ManifestError {
                file: manifest_path.to_path_buf(),
                span: Some(e.span.into()),
                message: format!("TOML syntax error: {e}"),
            });
            return None;
        }
    };

    let root_span = root_value.span;
    let mut root_table = match root_value.take() {
        ValueInner::Table(table) => table,
        _ => {
            errors.push(ManifestError {
                file: manifest_path.to_path_buf(),
                span: Some(root_span.into()),
                message: "manifest root must be a table".to_string(),
            });
            return None;
        }
    };

    let Some(mut link_value) = root_table.remove("link") else {
        return Some(Vec::new());
    };
    let link_span = link_value.span;
    let ValueInner::Table(link_table) = link_value.take() else {
        errors.push(ManifestError {
            file: manifest_path.to_path_buf(),
            span: Some(link_span.into()),
            message: "`[link]` must be a table".to_string(),
        });
        return Some(Vec::new());
    };

    let mut th = TableHelper::from((link_table, link_span));
    let includes: Option<Vec<WithSpan<String>>> = th.optional("includes");
    let mut diags = Diagnostics { file: manifest_path, errors };
    if let Err(err) = th.finalize(None) {
        diags.push_deser_error(err);
    }
    Some(includes.unwrap_or_default())
}

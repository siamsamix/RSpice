//! `.include` / `.lib` — splice library decks into the parse pipeline.

use super::*;

// ─── Library files (.include / .lib) ────────────────────────────────────────

/// Resolve a `.include`/`.lib` argument against the containing file's
/// directory, the configured search paths, and finally the process
/// directory — the PSpice `nom.lib`-respecting search order spelled out in
/// TODO §2. `None` when no candidate exists as a file.
pub(super) fn find_library_file(
    spec: &str,
    base: Option<&Path>,
    options: &ParseOptions,
) -> Option<PathBuf> {
    let bare = spec.trim().trim_matches(|c| c == '"' || c == '\'');
    if bare.is_empty() {
        return None;
    }
    let raw = PathBuf::from(bare);
    if raw.is_absolute() {
        return raw.is_file().then_some(raw);
    }
    if let Some(dir) = base {
        let candidate = dir.join(&raw);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    for dir in &options.search_paths {
        let candidate = dir.join(&raw);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // Relative to the process working directory last.
    if raw.is_file() {
        return Some(raw);
    }
    None
}

/// Read a deck or library file. Gated: the wasm build has no file system,
/// so it reports the platform limitation as the read error.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn open_deck_file(path: &Path) -> std::io::Result<(String, Option<PathBuf>)> {
    Ok((
        std::fs::read_to_string(path)?,
        path.parent().map(Path::to_path_buf),
    ))
}

#[cfg(target_arch = "wasm32")]
pub(super) fn open_deck_file(path: &Path) -> std::io::Result<(String, Option<PathBuf>)> {
    Err(std::io::Error::other(format!(
        "library file '{}' is not readable on the web build",
        path.display()
    )))
}

/// One library file opened and read, with the containing directory for the
/// next level of `.include`/`.lib` recursion.
pub(super) struct LibLoad {
    content: String,
    base: Option<PathBuf>,
    path: PathBuf,
}

pub(super) fn open_library(
    spec: &str,
    base: Option<&Path>,
    options: &ParseOptions,
) -> Result<Option<LibLoad>> {
    match find_library_file(spec, base, options) {
        Some(path) => {
            let (content, child_base) = open_deck_file(&path).map_err(|e| {
                SimError::Parse(format!("library file '{}': {e}", path.display()))
            })?;
            Ok(Some(LibLoad { content, base: child_base, path }))
        }
        None => Ok(None),
    }
}

/// Splice file-backed directives into a logical-line stream.
pub(super) struct SpliceCtx<'a> {
    options: &'a ParseOptions,
    /// Canonical paths of files currently open — a chain, not a set: sibling
    /// decks may load the same library twice, but a file may not contain
    /// itself.
    visited: Vec<String>,
    base: Option<PathBuf>,
}

pub(super) fn splice_libraries(
    logical: Vec<(usize, String)>,
    options: &ParseOptions,
) -> Result<Vec<(usize, String)>> {
    let mut ctx = SpliceCtx {
        options,
        visited: Vec::new(),
        base: options.deck_dir.clone(),
    };
    splice_lines(logical, &mut ctx, false)
}

/// Splice file-backed directives into a logical-line stream:
///
/// * `.include <file>` / `.inc <file>` — the file's logical lines are inserted
///   in the directive's place and processed recursively (nested
///   `.include`/`.lib` lines resolve relative to the *containing* file's
///   directory).
/// * `.lib <file> [<section>]` — same, but the PSpice section selection cuts
///   the `.lib <name>` … `.endl [<name>]` block out of the file first.
/// * A `.lib <word>` that names no file opens a section *boundary* inside a
///   library file. Boundaries never enter the deck stream: a whole-file read
///   merges every section (the LTspice behavior), a selected one is extracted
///   before the recursion.
/// * A library file's own `.end` terminates the file, not the importing deck.
/// * Resolution follows the containing directory, [`ParseOptions::search_paths`],
///   then the process directory; cycles fail loudly.
pub(super) fn splice_lines(
    lines: Vec<(usize, String)>,
    ctx: &mut SpliceCtx<'_>,
    in_lib: bool,
) -> Result<Vec<(usize, String)>> {
    let mut out = Vec::with_capacity(lines.len());
    for (line_no, line) in lines {
        let lower = line.to_ascii_lowercase();
        let cmd = lower.split_whitespace().next().unwrap_or("").to_string();
        let is_directive = matches!(
            cmd.as_str(),
            ".include" | ".inc" | ".lib"
        );
        if !is_directive {
            // A library file's own `.end` terminates the file, not the
            // importing deck. `.endl` marks the end of a section in the
            // same way: it is library structure, never deck content.
            if !in_lib || (cmd != ".end" && cmd != ".endl") {
                out.push((line_no, line));
            }
            continue;
        }

        if cmd == ".lib" {
            // One important overload: a `.lib` line we cannot resolve to a
            // file is a section boundary INSIDE a library (`.lib <name>` …
            // `.endl`). It defines structure, never content.
            let filespec = rest_args(&line).first().cloned().unwrap_or_default();
            if filespec.is_empty() {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .lib requires a file argument"
                )));
            }
            let section = rest_args(&line).get(1).map(|s| {
                s.trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase()
            });
            if let Some(load) = open_library(&filespec, ctx.base.as_deref(), ctx.options)? {
                splice_loaded_library(load, section.as_deref(), line_no, ctx, &mut out)?;
            } else {
                if in_lib {
                    continue; // a section boundary inside the library
                }
                let message = if section.is_some() {
                    format!(
                        "line {line_no}: .lib '{}' (section {:?}) — library file not found in search paths",
                        filespec,
                        section.unwrap_or_default()
                    )
                } else {
                    format!(
                        "line {line_no}: .lib '{}' — library file not found in search paths",
                        filespec
                    )
                };
                return Err(SimError::Parse(message));
            }
            continue;
        }

        // `.include`/`.inc`: splice the file's logical lines in place.
        let filespec = rest_args(&line).first().cloned().unwrap_or_default();
        if filespec.is_empty() {
            return Err(SimError::Parse(format!(
                "line {line_no}: .include requires a file argument"
            )));
        }
        let Some(load) = open_library(&filespec, ctx.base.as_deref(), ctx.options)? else {
            return Err(SimError::Parse(format!(
                "line {line_no}: .include '{}' — file not found in search paths",
                filespec
            )));
        };
        splice_loaded_library(load, None, line_no, ctx, &mut out)?;
    }
    Ok(out)
}

pub(super) fn rest_args(line: &str) -> Vec<String> {
    line.split_whitespace()
        .skip(1)
        .map(|t| t.trim_matches(|c| c == '"' || c == '\'').to_string())
        .collect()
}

/// Read, section-select, cycle-check, recursively splice one loaded file.
pub(super) fn splice_loaded_library(
    load: LibLoad,
    section: Option<&str>,
    deck_line: usize,
    ctx: &mut SpliceCtx<'_>,
    out: &mut Vec<(usize, String)>,
) -> Result<()> {
    let canonical = load
        .path
        .canonicalize()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| load.path.to_string_lossy().to_string());
    if ctx.visited.iter().any(|v| v == &canonical) {
        return Err(SimError::Parse(format!(
            "circular library chain at '{}'",
            load.path.display()
        )));
    }
    ctx.visited.push(canonical);

    // Note: the loaded file goes through the same logical-line cleaning as
    // the deck, so comments and `+` continuations inside libraries behave.
    let child_lines = logical_lines(&load.content)?;

    let child_lines = match section {
        Some(sec) => extract_section(&child_lines, sec, &load.path, deck_line)?,
        None => child_lines,
    };

    let parent_base = std::mem::replace(&mut ctx.base, load.base.clone());
    let spliced = splice_lines(child_lines, ctx, true)?;
    ctx.base = parent_base;
    ctx.visited.pop();

    out.extend(spliced);
    Ok(())
}

/// The lines of the `.lib <name>` … `.endl [<name>]` section. A nested
/// `.lib` boundary while the target section is open is a structural error.
pub(super) fn extract_section(
    lines: &[(usize, String)],
    name: &str,
    file: &Path,
    deck_line: usize,
) -> Result<Vec<(usize, String)>> {
    let mut body: Vec<(usize, String)> = Vec::new();
    let mut open = false;
    for (line_no, line) in lines {
        let first = line
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if !open {
            if first == ".lib" {
                let boundary = rest_args(line).first().map(|s| {
                    s.trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase()
                });
                if boundary.as_deref() == Some(name) {
                    open = true;
                }
            }
            continue;
        }
        if first == ".endl" {
            return Ok(body);
        }
        if first == ".lib" {
            return Err(SimError::Parse(format!(
                ".lib '{}' from line {deck_line}: nested .lib inside section '{name}' of {} — .lib sections do not nest",
                file.display(),
                name
            )));
        }
        body.push((*line_no, line.clone()));
    }
    Err(SimError::Parse(format!(
        ".lib file {}: section '{name}' has no .endl",
        file.display()
    )))
}

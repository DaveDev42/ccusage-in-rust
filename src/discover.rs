use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use walkdir::WalkDir;

const PROJECTS_DIR: &str = "projects";
const ENV_VAR: &str = "CLAUDE_CONFIG_DIR";

/// Returns the list of Claude config directories to scan.
///
/// Mirrors ccusage `getClaudePaths()`:
/// - If `CLAUDE_CONFIG_DIR` is set: comma-separated list of base dirs (each must contain `projects/`)
/// - Otherwise: `$XDG_CONFIG_HOME/claude` (or `~/.config/claude`) + `~/.claude`, each kept only if `projects/` exists.
pub(crate) fn claude_paths() -> Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    if let Ok(env_val) = env::var(ENV_VAR) {
        let env_val = env_val.trim();
        if !env_val.is_empty() {
            for raw in env_val.split(',') {
                let p = raw.trim();
                if p.is_empty() {
                    continue;
                }
                let abs = absolutize(Path::new(p));
                if abs.is_dir() && abs.join(PROJECTS_DIR).is_dir() && seen.insert(abs.clone()) {
                    out.push(abs);
                }
            }
            if out.is_empty() {
                return Err(anyhow!(
                    "No valid Claude data directories found in {ENV_VAR}. Each entry must contain a 'projects/' subdirectory.\n  Got: {env_val}"
                ));
            }
            return Ok(out);
        }
    }

    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not determine $HOME"))?;
    let xdg = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));

    let candidates = [xdg.join("claude"), home.join(".claude")];
    for c in candidates {
        if c.is_dir() && c.join(PROJECTS_DIR).is_dir() && seen.insert(c.clone()) {
            out.push(c);
        }
    }

    if out.is_empty() {
        return Err(anyhow!(
            "No valid Claude data directories found. Expected one of:\n  - {}/projects\n  - {}/projects\nor set {ENV_VAR}.",
            xdg.join("claude").display(),
            home.join(".claude").display()
        ));
    }

    Ok(out)
}

fn absolutize(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else if let Ok(cwd) = env::current_dir() {
        cwd.join(p)
    } else {
        p.to_path_buf()
    }
}

/// Result of walking a single base path's `projects/` tree.
pub(crate) struct DiscoveredFile {
    pub path: PathBuf,
    /// `<base>/projects/` — the directory we're rooted at. Used to compute relative project/session ids.
    pub base_dir: PathBuf,
}

pub(crate) fn discover_jsonl_files(bases: &[PathBuf]) -> Vec<DiscoveredFile> {
    let mut out = Vec::new();
    for base in bases {
        let projects = base.join(PROJECTS_DIR);
        if !projects.is_dir() {
            continue;
        }
        let mut per_base: Vec<DiscoveredFile> = Vec::new();
        for entry in WalkDir::new(&projects)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "jsonl") {
                per_base.push(DiscoveredFile {
                    path: p.to_path_buf(),
                    base_dir: projects.clone(),
                });
            }
        }
        // tinyglobby returns matches sorted by full relative-path string with `/` as
        // separator (so e.g. `-Users-dave--cache.../file` sorts before `-Users-dave/sub/file`
        // because `-` < `/` at position 11). Match that exactly.
        per_base.sort_by(|a, b| a.path.as_os_str().cmp(b.path.as_os_str()));
        out.extend(per_base);
    }
    out
}

/// Compute (sessionId, projectPath) for the session aggregator from a JSONL path within a base dir.
///
/// Mirrors ccusage's relative-path arithmetic:
///   parts = relative(base, file).split(sep)
///   sessionId = parts[parts.length - 2]   // the directory containing the file
///   projectPath = parts.slice(0, -2).join(sep)  // empty → "Unknown Project"
pub(crate) fn session_and_project(file: &Path, base_dir: &Path) -> (String, String) {
    let rel = file.strip_prefix(base_dir).unwrap_or(file);
    let parts: Vec<&std::ffi::OsStr> = rel.iter().collect();
    if parts.len() < 2 {
        return ("unknown".to_string(), "Unknown Project".to_string());
    }
    let session = parts[parts.len() - 2].to_string_lossy().into_owned();
    let project_parts = &parts[..parts.len() - 2];
    let project = if project_parts.is_empty() {
        "Unknown Project".to_string()
    } else {
        project_parts
            .iter()
            .map(|p| p.to_string_lossy())
            .collect::<Vec<_>>()
            .join(std::path::MAIN_SEPARATOR_STR)
    };
    (session, project)
}

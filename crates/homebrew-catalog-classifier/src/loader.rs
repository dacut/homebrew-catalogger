//! Locating and reading Homebrew's cached formula metadata.

use {
    crate::{Error, HomebrewFormulaMeta, Result},
    serde_json::Value,
    std::{
        collections::BTreeMap,
        ffi::OsString,
        fs,
        path::{Path, PathBuf},
    },
};

/// Bulk formula metadata files Homebrew may keep in its API cache, in the order they are read.
const BULK_FILES: [&str; 2] = ["formula.jws.json", "formula.json"];

/// Returns Homebrew's cache directory for this platform (what `brew --cache` prints).
///
/// This is `$HOMEBREW_CACHE` when set; otherwise `~/Library/Caches/Homebrew` on macOS and
/// `${XDG_CACHE_HOME:-~/.cache}/Homebrew` elsewhere.
pub fn default_homebrew_cache_dir() -> Option<PathBuf> {
    cache_dir_from(|key| std::env::var_os(key), std::env::home_dir(), cfg!(target_os = "macos"))
}

/// Returns the directory holding Homebrew's cached API metadata (`<cache>/api`).
pub fn default_api_cache_dir() -> Option<PathBuf> {
    default_homebrew_cache_dir().map(|dir| dir.join("api"))
}

fn cache_dir_from(env: impl Fn(&str) -> Option<OsString>, home: Option<PathBuf>, macos: bool) -> Option<PathBuf> {
    let non_empty = |key| env(key).filter(|v| !v.is_empty());

    if let Some(dir) = non_empty("HOMEBREW_CACHE") {
        return Some(dir.into());
    }

    if macos {
        return home.map(|h| h.join("Library").join("Caches").join("Homebrew"));
    }

    let xdg = non_empty("XDG_CACHE_HOME").map(PathBuf::from).filter(|p| p.is_absolute());
    xdg.or_else(|| home.map(|h| h.join(".cache"))).map(|dir| dir.join("Homebrew"))
}

/// Loads every formula from Homebrew's default metadata location.
///
/// See [`default_api_cache_dir`] and [`load_formulae`].
pub fn load_default_formulae() -> Result<Vec<HomebrewFormulaMeta>> {
    load_formulae(default_api_cache_dir().ok_or(Error::NoDefaultCacheDir)?)
}

/// Loads every formula found at `path`.
///
/// `path` may be:
///
/// * Homebrew's cache directory (`brew --cache`) or its `api` subdirectory. Both the bulk `formula.jws.json` /
///   `formula.json` files and the per-formula files under `formula/` are read; when a formula appears in both, the
///   per-formula file wins.
/// * A directory of per-formula `*.json` files.
/// * A single file containing one formula object, a JSON array of formulae, or a JWS envelope (`{"payload": ...}`)
///   wrapping either of those.
///
/// The result is sorted by formula name.
pub fn load_formulae(path: impl AsRef<Path>) -> Result<Vec<HomebrewFormulaMeta>> {
    let path = path.as_ref();
    let metadata = fs::metadata(path).map_err(|source| io_error(path, source))?;
    if metadata.is_file() {
        return read_formula_file(path);
    }

    let api_dir = resolve_api_dir(path);
    let mut by_name = BTreeMap::new();
    let mut found_bulk = false;

    for bulk in BULK_FILES {
        let bulk_path = api_dir.join(bulk);
        if bulk_path.is_file() {
            found_bulk = true;
            for formula in read_formula_file(&bulk_path)? {
                by_name.insert(formula.name.clone(), formula);
            }
        }
    }

    let per_formula_dir = api_dir.join("formula");
    if per_formula_dir.is_dir() {
        read_json_dir(&per_formula_dir, &mut by_name)?;
    } else if !found_bulk {
        read_json_dir(&api_dir, &mut by_name)?;
    }

    Ok(by_name.into_values().collect())
}

/// Finds a single formula in Homebrew's default metadata location.
///
/// See [`default_api_cache_dir`] and [`find_formula`].
pub fn find_default_formula(name: &str) -> Result<HomebrewFormulaMeta> {
    find_formula(name, default_api_cache_dir().ok_or(Error::NoDefaultCacheDir)?)
}

/// Finds the formula called `name` in the metadata at `path`.
///
/// `path` accepts the same forms as [`load_formulae`]. `name` may be a formula's name, full name, alias, or former
/// name. When `path` is a Homebrew cache directory, the formula's own `formula/<name>.json` file is read directly
/// instead of loading everything.
pub fn find_formula(name: &str, path: impl AsRef<Path>) -> Result<HomebrewFormulaMeta> {
    let path = path.as_ref();

    if path.is_dir() {
        let api_dir = resolve_api_dir(path);
        let canonical = resolve_alias(&api_dir, name).unwrap_or_else(|| name.to_string());
        if is_plain_file_name(&canonical) {
            let file = api_dir.join("formula").join(format!("{canonical}.json"));
            if file.is_file()
                && let Some(formula) = read_formula_file(&file)?.into_iter().find(|f| f.is_named(&canonical))
            {
                return Ok(formula);
            }
        }
    }

    load_formulae(path)?.into_iter().find(|f| f.is_named(name)).ok_or_else(|| Error::FormulaNotFound(name.to_string()))
}

/// Reads a single metadata file, returning every formula it contains.
pub fn read_formula_file(path: impl AsRef<Path>) -> Result<Vec<HomebrewFormulaMeta>> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|source| io_error(path, source))?;
    let mut value: Value = serde_json::from_slice(&bytes).map_err(|source| json_error(path, source))?;

    // Homebrew signs its bulk API files as JSON Web Signatures; the formula data is a JSON string in `payload`.
    if let Some(Value::String(payload)) = value.get("payload") {
        value = serde_json::from_str(payload).map_err(|source| json_error(path, source))?;
    }

    match value {
        Value::Array(items) => items
            .into_iter()
            .map(|item| serde_json::from_value(item).map_err(|source| json_error(path, source)))
            .collect(),
        Value::Object(ref obj) if obj.get("name").is_some_and(Value::is_string) => {
            Ok(vec![serde_json::from_value(value).map_err(|source| json_error(path, source))?])
        }
        _ => Err(Error::UnrecognizedMetadata {
            path: path.to_path_buf(),
            reason: "expected a formula object, an array of formulae, or a JWS envelope".to_string(),
        }),
    }
}

/// Accepts either the Homebrew cache root or its `api` subdirectory.
fn resolve_api_dir(path: &Path) -> PathBuf {
    let api = path.join("api");
    if api.is_dir() {
        api
    } else {
        path.to_path_buf()
    }
}

/// Looks `name` up in Homebrew's `formula_aliases.txt` (`alias|formula` per line).
fn resolve_alias(api_dir: &Path, name: &str) -> Option<String> {
    let aliases = fs::read_to_string(api_dir.join("formula_aliases.txt")).ok()?;
    aliases.lines().find_map(|line| {
        let (alias, target) = line.split_once('|')?;
        (alias == name).then(|| target.to_string())
    })
}

fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && !name.contains(['/', '\\'])
}

fn read_json_dir(dir: &Path, by_name: &mut BTreeMap<String, HomebrewFormulaMeta>) -> Result<()> {
    let entries = fs::read_dir(dir).map_err(|source| io_error(dir, source))?;
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| io_error(dir, source))?;
        let file = entry.path();
        if file.extension().is_some_and(|ext| ext == "json") && file.is_file() {
            files.push(file);
        }
    }
    files.sort();

    for file in files {
        for formula in read_formula_file(&file)? {
            by_name.insert(formula.name.clone(), formula);
        }
    }
    Ok(())
}

fn io_error(path: &Path, source: std::io::Error) -> Error {
    Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn json_error(path: &Path, source: serde_json::Error) -> Error {
    Error::Json {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use {super::*, std::collections::HashMap};

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let vars: HashMap<String, OsString> = vars.iter().map(|(k, v)| (k.to_string(), OsString::from(v))).collect();
        move |key| vars.get(key).cloned()
    }

    #[test]
    fn homebrew_cache_env_wins() {
        let dir = cache_dir_from(env(&[("HOMEBREW_CACHE", "/custom")]), Some("/home/u".into()), true);
        assert_eq!(dir, Some(PathBuf::from("/custom")));
    }

    #[test]
    fn macos_default() {
        let dir = cache_dir_from(env(&[("HOMEBREW_CACHE", "")]), Some("/Users/u".into()), true);
        assert_eq!(dir, Some(PathBuf::from("/Users/u/Library/Caches/Homebrew")));
    }

    #[test]
    fn linux_defaults() {
        let dir = cache_dir_from(env(&[]), Some("/home/u".into()), false);
        assert_eq!(dir, Some(PathBuf::from("/home/u/.cache/Homebrew")));

        let dir = cache_dir_from(env(&[("XDG_CACHE_HOME", "/xdg")]), Some("/home/u".into()), false);
        assert_eq!(dir, Some(PathBuf::from("/xdg/Homebrew")));

        // Relative XDG paths are invalid per the spec and ignored.
        let dir = cache_dir_from(env(&[("XDG_CACHE_HOME", "rel")]), Some("/home/u".into()), false);
        assert_eq!(dir, Some(PathBuf::from("/home/u/.cache/Homebrew")));

        assert_eq!(cache_dir_from(env(&[]), None, false), None);
    }
}

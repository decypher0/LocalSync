//! Files outside the project that a config value points at (an absolute path
//! in `application.properties`, say). Detection is read-only; the chosen
//! files travel in the snapshot under `external/<id>/`.
//!
//! How a chosen file reaches the app on the receiver:
//! - it is bundled at the payload root, `external/<id>/...` (next to
//!   `db-dumps/`), streamed from where it is on this computer;
//! - the generated compose file mounts `../external/<id>` read-only into the
//!   app container at `/localsync/external/<id>`;
//! - LocalSync's copy of the config file (the one in the payload's `source/`,
//!   i.e. the build context) has that key's value changed to the in-container
//!   path before anything is built - so a Java `src/main/resources` file is
//!   already right when it is packed into the jar. The person's own file is
//!   never written.
//!
//! Rewriting covers `.properties`, `.env` and YAML. XML is detected but not
//! rewritten yet (the file is still mounted; a note says where).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use ls_snapshot::ExternalKind;

/// Above this, a file or folder is listed unticked with a size warning: it
/// is copied into every send.
pub const SIZE_WARN_BYTES: u64 = 50 * 1024 * 1024;
/// Where the files are mounted inside the app container.
pub const CONTAINER_ROOT: &str = "/localsync/external";
/// One plain line for what isn't handled yet (shown in Setup).
pub const NOT_SUPPORTED_YET: &str =
    "Not supported yet: changing paths inside XML files, values split over several lines, and projects with their own docker-compose.yml.";

/// Build output, dependencies and tool folders: never scanned.
const SKIP_DIRS: &[&str] = &[
    "target", "node_modules", "build", "dist", "out", ".git", ".svn", ".hg", "venv", ".venv", "__pycache__", ".tox", ".gradle", ".idea", ".vscode", ".next",
];
/// Config files bigger than this aren't scanned (they aren't hand-written config).
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
const UNIX_ROOTS: &[&str] = &["/home", "/Users", "/root", "/usr", "/opt", "/var", "/etc", "/mnt", "/srv"];

/// Where an absolute path found in a config file is, on this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalStatus {
    /// Inside the project: a relative path would do.
    Inside,
    /// Outside the project and present: can be bundled.
    OutsideExists,
    /// Outside the project and missing here: can't be bundled.
    OutsideMissing,
}

/// One active config value that is an absolute filesystem path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExternalRef {
    /// From the path: every reference to the same file or folder shares it,
    /// so it is bundled and mounted once.
    pub id: String,
    pub key: String,
    /// Relative to the project root, `/`-separated. Empty for a file added
    /// by hand whose key no config file sets.
    pub config_file: String,
    /// The value exactly as the config file writes it.
    pub value: String,
    /// The path on this computer the value resolves to.
    pub path: String,
    pub kind: ExternalKind,
    pub status: ExternalStatus,
    /// For a folder, counting stops a little past [`SIZE_WARN_BYTES`].
    #[serde(default)]
    pub size_bytes: u64,
    /// For [`ExternalStatus::Inside`]: the same path relative to the project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_relative: Option<String>,
    /// 1-based line of the value in `config_file` (0 when unknown).
    #[serde(default)]
    pub line: usize,
    /// Where the app sees it in the container once sent.
    #[serde(default)]
    pub container_path: String,
    /// LocalSync can change the value in its copy of `config_file`.
    #[serde(default)]
    pub rewritable: bool,
}

/// An active `key = absolute path` line in one config file's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathHit {
    pub key: String,
    /// As written, without surrounding quotes (escapes kept). [`hit_path`]
    /// turns it into the filesystem path.
    pub value: String,
    /// 1-based.
    pub line: usize,
}

/// Active (not commented-out) values in `text` that are absolute
/// filesystem paths. `config_file` picks the syntax (.properties, .yml,
/// .env, .xml); any other file name gives nothing.
pub fn find_absolute_paths(config_file: &str, text: &str) -> Vec<PathHit> {
    entries(config_file, text)
        .into_iter()
        .filter(|e| hit_path(config_file, &e.value).is_some())
        .map(|e| PathHit { key: e.key, value: e.value, line: e.line })
        .collect()
}

/// The filesystem path a value found by [`find_absolute_paths`] names
/// (escapes undone, `file:` URL unwrapped), or `None` if it isn't an
/// absolute path.
pub fn hit_path(config_file: &str, value: &str) -> Option<String> {
    let syntax = syntax_of(config_file)?;
    absolute_path(&unescape(syntax, value))
}

/// Scans a project folder's config files for values that point outside it.
#[tauri::command]
pub async fn detect_external_refs(folder_path: String) -> Result<Vec<ExternalRef>, String> {
    tauri::async_runtime::spawn_blocking(move || scan(Path::new(&folder_path)))
        .await
        .map_err(|e| format!("scan task panicked: {e}"))?
}

/// "Add a file or folder...": describes `path` as an external file the
/// config `key` points at (the key is looked up in the folder's config
/// files; it needn't be there).
#[tauri::command]
pub async fn describe_external(folder_path: String, key: String, path: String) -> Result<ExternalRef, String> {
    tauri::async_runtime::spawn_blocking(move || describe(Path::new(&folder_path), key.trim(), &path))
        .await
        .map_err(|e| format!("task panicked: {e}"))?
}

/// A content fingerprint of the chosen external files (None when there are
/// none): any change to a file's bytes, a folder's files, or which files are
/// chosen changes it. For change detection next to the project's commits.
pub fn externals_fingerprint(refs: &[ExternalRef]) -> Option<String> {
    if refs.is_empty() {
        return None;
    }
    let mut sorted: Vec<&ExternalRef> = refs.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    let mut h = Sha256::new();
    for r in sorted {
        h.update(format!("{}\0{}\0", r.id, r.path));
        let path = Path::new(&r.path);
        let files = if path.is_dir() { ls_files(path) } else { vec![path.to_path_buf()] };
        for f in files {
            h.update(f.strip_prefix(path).unwrap_or(&f).to_string_lossy().as_bytes());
            match std::fs::File::open(&f) {
                // ponytail: hashes every byte on each call; key on mtime+size if folders get big.
                Ok(mut file) => {
                    let _ = std::io::copy(&mut file, &mut h);
                }
                Err(_) => h.update(b"\0missing\0"),
            }
        }
    }
    Some(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// [`externals_fingerprint`] of each chosen file or folder on its own, by
/// id: change detection names which one changed.
pub fn externals_fingerprints(refs: &[ExternalRef]) -> std::collections::BTreeMap<String, String> {
    refs.iter().filter_map(|r| Some((r.id.clone(), externals_fingerprint(std::slice::from_ref(r))?))).collect()
}

/// What the snapshot bundles for `refs`: each present file/folder outside
/// the project, with where it is now.
pub fn bundle_entries(refs: &[ExternalRef]) -> Vec<(ls_snapshot::ExternalFile, PathBuf)> {
    refs.iter()
        .filter(|r| r.status == ExternalStatus::OutsideExists)
        .map(|r| {
            let bundle_path = match r.kind {
                ExternalKind::File => format!("external/{}/{}", r.id, bundle_name(&r.path)),
                ExternalKind::Dir => format!("external/{}", r.id),
            };
            let file = ls_snapshot::ExternalFile {
                id: r.id.clone(),
                key: r.key.clone(),
                config_file: r.config_file.clone(),
                original_value: r.value.clone(),
                kind: r.kind,
                bundle_path,
            };
            (file, PathBuf::from(&r.path))
        })
        .collect()
}

/// Adds the chosen externals of the project at `folder` to what the compose
/// wizard generated: a read-only mount per file/folder on the app service,
/// LocalSync's rewritten copy of each config file that points at one (as a
/// generated file, so it replaces the project's copy in the build context),
/// and a note for anything it couldn't rewrite.
pub fn apply_to_generated(folder: &Path, refs: &[ExternalRef], g: &mut ls_composegen::Generated) -> Result<(), String> {
    let sent: Vec<&ExternalRef> = refs.iter().filter(|r| r.status == ExternalStatus::OutsideExists).collect();
    let mut mounts: Vec<String> = sent.iter().map(|r| format!("../external/{}:{CONTAINER_ROOT}/{}:ro", r.id, r.id)).collect();
    mounts.sort();
    mounts.dedup();
    g.compose_yaml = ls_composegen::generate::add_app_volumes(&g.compose_yaml, &mounts).map_err(|e| format!("{e:?}"))?;

    let mut configs: Vec<(String, String)> = Vec::new(); // (config_file, LocalSync's copy)
    for r in sent {
        let to = container_value(r);
        if r.config_file.is_empty() {
            g.notes.push(format!("{} is in the app's container at {to}.", bundle_name(&r.path)));
            continue;
        }
        if !is_rewritable(&r.config_file) {
            g.notes.push(format!(
                "{} in {} still says {}: LocalSync can't change XML files yet. The file is in the app's container at {to}.",
                r.key, r.config_file, r.value
            ));
            continue;
        }
        let idx = match configs.iter().position(|(f, _)| *f == r.config_file) {
            Some(i) => i,
            None => {
                let text = std::fs::read_to_string(folder.join(&r.config_file))
                    .map_err(|e| format!("Couldn't read {}: {e}", r.config_file))?;
                configs.push((r.config_file.clone(), text));
                configs.len() - 1
            }
        };
        let (from, text) = rewrite_value(&r.config_file, &configs[idx].1, &r.key, &r.value, &to)?;
        configs[idx].1 = text;
        g.rewrites.push(ls_composegen::Rewrite { key: format!("{} ({})", r.key, r.config_file), from, to });
    }
    for (path, contents) in configs {
        g.files.push(ls_composegen::GeneratedFile { path, contents });
    }
    Ok(())
}

/// `text` with the active value of `key` replaced by `new_value`, and the
/// value it replaced. When the key is set more than once, the line whose
/// value is `old_value` wins. Only syntaxes [`is_rewritable`] accepts.
pub fn rewrite_value(config_file: &str, text: &str, key: &str, old_value: &str, new_value: &str) -> Result<(String, String), String> {
    if !is_rewritable(config_file) {
        return Err(format!("LocalSync can't change {config_file} yet."));
    }
    let all = entries(config_file, text);
    let same_key: Vec<&Entry> = all.iter().filter(|e| e.key == key).collect();
    let entry = match same_key.iter().find(|e| e.value == old_value) {
        Some(e) => *e,
        None if same_key.len() == 1 => same_key[0],
        _ => return Err(format!("{key} is no longer set once in {config_file}. Check Files outside the project again.")),
    };
    let mut out = String::with_capacity(text.len() + new_value.len());
    for (i, raw) in text.split_inclusive('\n').enumerate() {
        if i + 1 == entry.line {
            let end = entry.col + entry.value.len();
            out.push_str(&raw[..entry.col]);
            out.push_str(new_value);
            out.push_str(&raw[end..]);
        } else {
            out.push_str(raw);
        }
    }
    Ok((entry.value.clone(), out))
}

/// LocalSync can change values in this kind of config file.
pub fn is_rewritable(config_file: &str) -> bool {
    matches!(syntax_of(config_file), Some(Syntax::Properties | Syntax::Yaml | Syntax::Env))
}

// ---------- scanning ----------

fn scan(root: &Path) -> Result<Vec<ExternalRef>, String> {
    if !root.is_dir() {
        return Err(format!("{} is not a folder.", root.display()));
    }
    let mut out = Vec::new();
    for file in config_files(root) {
        let Ok(meta) = std::fs::metadata(&file) else { continue };
        if meta.len() > MAX_CONFIG_BYTES {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        let rel = rel_slash(root, &file);
        for hit in find_absolute_paths(&rel, &text) {
            let path = hit_path(&rel, &hit.value).unwrap_or_default();
            out.push(make_ref(root, &rel, hit.key, hit.value, hit.line, path));
        }
    }
    Ok(out)
}

fn describe(root: &Path, key: &str, path: &str) -> Result<ExternalRef, String> {
    if key.is_empty() {
        return Err("Enter the setting (key) that points at it.".into());
    }
    if !Path::new(path).exists() {
        return Err(format!("{path} doesn't exist."));
    }
    let found = config_files(root).into_iter().find_map(|f| {
        let rel = rel_slash(root, &f);
        let text = std::fs::read_to_string(&f).ok()?;
        entries(&rel, &text).into_iter().find(|e| e.key == key).map(|e| (rel, e))
    });
    let r = match found {
        Some((rel, e)) => make_ref(root, &rel, key.to_string(), e.value, e.line, path.to_string()),
        None => make_ref(root, "", key.to_string(), String::new(), 0, path.to_string()),
    };
    Ok(r)
}

fn make_ref(root: &Path, config_file: &str, key: String, value: String, line: usize, path: String) -> ExternalRef {
    let id = make_id(&path);
    let p = Path::new(&path);
    let trailing_sep = path.ends_with('/') || path.ends_with('\\');
    let inside = p.is_absolute() && is_inside(root, p);
    let meta = if p.is_absolute() { std::fs::metadata(p).ok() } else { None };
    let kind = match &meta {
        Some(m) if m.is_dir() => ExternalKind::Dir,
        Some(_) => ExternalKind::File,
        None if trailing_sep => ExternalKind::Dir,
        None => ExternalKind::File,
    };
    let status = match (&meta, inside) {
        (_, true) => ExternalStatus::Inside,
        (Some(_), false) => ExternalStatus::OutsideExists,
        (None, false) => ExternalStatus::OutsideMissing,
    };
    let size_bytes = match &meta {
        Some(m) if m.is_dir() => dir_size(p, SIZE_WARN_BYTES + 1),
        Some(m) => m.len(),
        None => 0,
    };
    let suggested_relative = inside.then(|| rel_slash(&canonical(root), &canonical(p)));
    let mut r = ExternalRef {
        id,
        key,
        config_file: config_file.to_string(),
        value,
        path,
        kind,
        status,
        size_bytes,
        suggested_relative,
        line,
        container_path: String::new(),
        rewritable: !config_file.is_empty() && is_rewritable(config_file),
    };
    r.container_path = container_path(&r);
    r
}

/// Short, stable, safe as a folder name: the file name's words plus a hash
/// of the whole path.
fn make_id(path: &str) -> String {
    let mut slug: String = bundle_name(path).chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    slug = slug.split('-').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("-");
    slug.truncate(40);
    let digest = Sha256::digest(path);
    let short: String = digest.iter().take(3).map(|b| format!("{b:02x}")).collect();
    if slug.is_empty() { format!("ext-{short}") } else { format!("{slug}-{short}") }
}

fn container_path(r: &ExternalRef) -> String {
    match r.kind {
        ExternalKind::File => format!("{CONTAINER_ROOT}/{}/{}", r.id, bundle_name(&r.path)),
        ExternalKind::Dir => format!("{CONTAINER_ROOT}/{}", r.id),
    }
}

/// The value LocalSync's copy of the config gets: the in-container path,
/// keeping a `file:` scheme and a folder's trailing slash.
fn container_value(r: &ExternalRef) -> String {
    let mut v = container_path(r);
    if r.kind == ExternalKind::Dir && (r.value.ends_with('/') || r.value.ends_with('\\')) {
        v.push('/');
    }
    if r.value.trim_start().starts_with("file:") {
        v = format!("file:{v}");
    }
    v
}

/// The file's name, with anything that would need quoting or escaping in a
/// config value replaced (spaces, quotes, `#`...).
fn bundle_name(path: &str) -> String {
    let name = path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or("file");
    let safe: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' }).collect();
    if safe.is_empty() || safe.chars().all(|c| c == '.') { "file".into() } else { safe }
}

fn config_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            let name = entry.file_name().to_string_lossy().to_string();
            if ft.is_dir() {
                if !SKIP_DIRS.contains(&name.as_str()) {
                    stack.push(entry.path());
                }
            } else if ft.is_file() && syntax_of(&name).is_some() {
                out.push(entry.path());
            }
        }
    }
    out.sort();
    out
}

fn ls_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for entry in rd.flatten() {
            match entry.file_type() {
                Ok(ft) if ft.is_dir() => stack.push(entry.path()),
                Ok(ft) if ft.is_file() => out.push(entry.path()),
                _ => {}
            }
        }
    }
    out.sort();
    out
}

/// Total size of the files under `dir`, counting stops once past `stop`.
fn dir_size(dir: &Path, stop: u64) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for entry in rd.flatten() {
            match entry.file_type() {
                Ok(ft) if ft.is_dir() => stack.push(entry.path()),
                Ok(ft) if ft.is_file() => total += entry.metadata().map(|m| m.len()).unwrap_or(0),
                _ => {}
            }
            if total > stop {
                return total;
            }
        }
    }
    total
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn is_inside(root: &Path, p: &Path) -> bool {
    let (root, p) = (canonical(root), canonical(p));
    if cfg!(windows) {
        // Drive letters and folder names are case-insensitive there.
        let lower = |x: &Path| x.to_string_lossy().replace('\\', "/").to_lowercase();
        let (r, q) = (lower(&root), lower(&p));
        q == r || q.starts_with(&format!("{}/", r.trim_end_matches('/')))
    } else {
        p.starts_with(&root)
    }
}

fn rel_slash(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).to_string_lossy().replace('\\', "/")
}

// ---------- parsing ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Syntax {
    Properties,
    Yaml,
    Env,
    Xml,
}

fn syntax_of(config_file: &str) -> Option<Syntax> {
    let name = config_file.rsplit(['/', '\\']).next()?.to_ascii_lowercase();
    if name.ends_with(".properties") {
        Some(Syntax::Properties)
    } else if name.ends_with(".yml") || name.ends_with(".yaml") {
        Some(Syntax::Yaml)
    } else if name == ".env" || name.starts_with(".env.") || name.ends_with(".env") {
        Some(Syntax::Env)
    } else if name.ends_with(".xml") {
        Some(Syntax::Xml)
    } else {
        None
    }
}

/// One active `key = value` in a config file.
#[derive(Debug, Clone)]
struct Entry {
    key: String,
    /// As written, quotes removed, escapes kept.
    value: String,
    /// 1-based.
    line: usize,
    /// Byte offset of `value` in its line.
    col: usize,
}

fn entries(config_file: &str, text: &str) -> Vec<Entry> {
    match syntax_of(config_file) {
        Some(Syntax::Properties) => properties_entries(text),
        Some(Syntax::Yaml) => yaml_entries(text),
        Some(Syntax::Env) => env_entries(text),
        Some(Syntax::Xml) => xml_entries(text),
        None => Vec::new(),
    }
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn properties_entries(text: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut continued = false;
    for (i, line) in text.lines().enumerate() {
        let was_continued = continued;
        let trailing = line.len() - line.trim_end_matches('\\').len();
        continued = trailing % 2 == 1;
        // ponytail: a value continued over several lines is skipped, not joined.
        if was_continued || continued {
            continue;
        }
        let body = line.trim_start();
        if body.is_empty() || body.starts_with('#') || body.starts_with('!') {
            continue;
        }
        let start = indent_of(line);
        let bytes = line.as_bytes();
        let mut k = start;
        while k < bytes.len() {
            match bytes[k] {
                b'\\' => k += 1 + line[k + 1..].chars().next().map_or(0, char::len_utf8),
                b'=' | b':' | b' ' | b'\t' => break,
                _ => k += 1,
            }
        }
        let k = k.min(bytes.len());
        let key = unescape(Syntax::Properties, &line[start..k]);
        let mut v = k;
        while v < bytes.len() && matches!(bytes[v], b' ' | b'\t') {
            v += 1;
        }
        if v < bytes.len() && matches!(bytes[v], b'=' | b':') {
            v += 1;
        }
        while v < bytes.len() && matches!(bytes[v], b' ' | b'\t') {
            v += 1;
        }
        let value = line[v..].trim_end().to_string();
        out.push(Entry { key, value, line: i + 1, col: v });
    }
    out
}

/// A quoted value's inner text and its offset within `s`, or `s` itself.
fn unquote_at(s: &str) -> (&str, usize) {
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') {
        if let Some(end) = s[1..].find(b[0] as char) {
            return (&s[1..1 + end], 1);
        }
    }
    (s, 0)
}

/// `s` up to an unquoted ` #` comment, trimmed at the end.
fn strip_trailing_comment(s: &str) -> &str {
    let mut quote = None;
    let b = s.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        match (quote, c) {
            (None, b'"' | b'\'') => quote = Some(c),
            (Some(q), _) if c == q => quote = None,
            (None, b'#') if i == 0 || b[i - 1] == b' ' || b[i - 1] == b'\t' => return s[..i].trim_end(),
            _ => {}
        }
    }
    s.trim_end()
}

fn yaml_entries(text: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut parents: Vec<(usize, String)> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let body = line.trim_start();
        if line.starts_with("---") || line.starts_with("...") {
            parents.clear();
            continue;
        }
        if body.is_empty() || body.starts_with('#') || body.starts_with('-') {
            continue;
        }
        let indent = indent_of(line);
        let Some(colon) = find_yaml_colon(body) else { continue };
        let key = unquote_at(body[..colon].trim()).0.to_string();
        while parents.last().is_some_and(|(ind, _)| *ind >= indent) {
            parents.pop();
        }
        let after = &body[colon + 1..];
        let rest = strip_trailing_comment(after.trim_start());
        if rest.is_empty() {
            parents.push((indent, key));
            continue;
        }
        if rest.starts_with(['|', '>', '{', '[', '&', '*', '!']) {
            continue;
        }
        let full_key = parents.iter().map(|(_, k)| k.as_str()).chain([key.as_str()]).collect::<Vec<_>>().join(".");
        let (value, q) = unquote_at(rest);
        let col = indent + colon + 1 + (after.len() - after.trim_start().len()) + q;
        out.push(Entry { key: full_key, value: value.to_string(), line: i + 1, col });
    }
    out
}

/// The `:` ending a YAML key: followed by a space or the end of the line,
/// outside quotes.
fn find_yaml_colon(body: &str) -> Option<usize> {
    let b = body.as_bytes();
    let mut quote = None;
    for (i, &c) in b.iter().enumerate() {
        match (quote, c) {
            (None, b'"' | b'\'') if i == 0 => quote = Some(c),
            (Some(q), _) if c == q => quote = None,
            (None, b':') if i + 1 == b.len() || b[i + 1] == b' ' || b[i + 1] == b'\t' => return Some(i),
            (None, b'#') if i > 0 && b[i - 1] == b' ' => return None,
            _ => {}
        }
    }
    None
}

fn env_entries(text: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let body = line.trim_start();
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        let mut start = indent_of(line);
        if body.starts_with("export ") {
            start += "export ".len();
        }
        let Some(eq) = line[start..].find('=') else { continue };
        let key = line[start..start + eq].trim().to_string();
        if key.is_empty() || key.contains(' ') {
            continue;
        }
        let after = &line[start + eq + 1..];
        let lead = after.len() - after.trim_start().len();
        let rest = after.trim_start();
        let (value, q) = if rest.starts_with(['"', '\'']) { unquote_at(rest) } else { (strip_trailing_comment(rest), 0) };
        out.push(Entry { key, value: value.to_string(), line: i + 1, col: start + eq + 1 + lead + q });
    }
    out
}

/// `<name>value</name>`, `<entry key="k">value</entry>` and
/// `<property name="k" value="v"/>`, each on one line, outside comments.
fn xml_entries(text: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut in_comment = false;
    for (i, raw) in text.lines().enumerate() {
        // Blank out comments, keeping byte offsets.
        let mut line = String::with_capacity(raw.len());
        let mut rest = raw;
        while !rest.is_empty() {
            if in_comment {
                match rest.find("-->") {
                    Some(e) => {
                        line.push_str(&" ".repeat(e + 3));
                        rest = &rest[e + 3..];
                        in_comment = false;
                    }
                    None => {
                        line.push_str(&" ".repeat(rest.len()));
                        rest = "";
                    }
                }
            } else {
                match rest.find("<!--") {
                    Some(s) => {
                        line.push_str(&rest[..s]);
                        line.push_str("    ");
                        rest = &rest[s + 4..];
                        in_comment = true;
                    }
                    None => {
                        line.push_str(rest);
                        rest = "";
                    }
                }
            }
        }
        let mut pos = 0;
        while let Some(lt) = line[pos..].find('<').map(|x| x + pos) {
            let Some(gt) = line[lt..].find('>').map(|x| x + lt) else { break };
            pos = gt + 1;
            let tag = &line[lt + 1..gt];
            if tag.starts_with(['/', '?', '!']) {
                continue;
            }
            let self_closing = tag.ends_with('/');
            let tag = tag.trim_end_matches('/');
            let name = tag.split_whitespace().next().unwrap_or("");
            let attrs = xml_attrs(tag, lt + 1);
            let attr = |n: &str| attrs.iter().find(|(k, _, _)| k == n);
            let key = attr("key").or_else(|| attr("name")).map(|(_, v, _)| v.clone());
            if let (Some(key), Some((_, v, col))) = (key.clone(), attr("value")) {
                out.push(Entry { key, value: v.clone(), line: i + 1, col: *col });
                continue;
            }
            if self_closing {
                continue;
            }
            let Some(next) = line[pos..].find('<').map(|x| x + pos) else { continue };
            if !line[next..].starts_with(&format!("</{name}>")) {
                continue;
            }
            let inner = &line[pos..next];
            let value = inner.trim();
            if !value.is_empty() {
                let col = pos + (inner.len() - inner.trim_start().len());
                out.push(Entry { key: key.unwrap_or_else(|| name.to_string()), value: value.to_string(), line: i + 1, col });
            }
        }
    }
    out
}

/// (name, value, byte offset of the value in the line) for each `a="v"`.
fn xml_attrs(tag: &str, offset: usize) -> Vec<(String, String, usize)> {
    let mut out = Vec::new();
    let mut rest = tag;
    let mut base = offset;
    while let Some(eq) = rest.find('=') {
        let name = rest[..eq].split_whitespace().last().unwrap_or("").to_string();
        let after = &rest[eq + 1..];
        let lead = after.len() - after.trim_start().len();
        let a = after.trim_start();
        let Some(q) = a.chars().next().filter(|c| *c == '"' || *c == '\'') else { break };
        let Some(end) = a[1..].find(q) else { break };
        let col = base + eq + 1 + lead + 1;
        out.push((name, a[1..1 + end].to_string(), col));
        let consumed = eq + 1 + lead + 1 + end + 1;
        rest = &rest[consumed..];
        base += consumed;
    }
    out
}

/// A value as written -> the string the app reads.
fn unescape(syntax: Syntax, value: &str) -> String {
    match syntax {
        Syntax::Properties => {
            let mut out = String::with_capacity(value.len());
            let mut chars = value.chars();
            while let Some(c) = chars.next() {
                if c != '\\' {
                    out.push(c);
                    continue;
                }
                match chars.next() {
                    Some('t') => out.push('\t'),
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('f') => out.push('\x0c'),
                    Some(other) => out.push(other),
                    None => {}
                }
            }
            out
        }
        // Inner text of quotes: only `\\` matters for a path.
        Syntax::Yaml | Syntax::Env => value.replace("\\\\", "\\"),
        Syntax::Xml => value.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&"),
    }
}

/// The filesystem path `value` names if it is absolute: a drive path
/// (`C:\`, `C:/`), one under a usual Unix root, or a `file:` URL to either.
fn absolute_path(value: &str) -> Option<String> {
    let v = value.trim();
    if v.contains("${") {
        return None;
    }
    let p = match v.strip_prefix("file:") {
        Some(rest) => {
            let rest = rest.strip_prefix("//").unwrap_or(rest);
            let rest = if rest.starts_with('/') && is_drive_path(&rest[1..]) { &rest[1..] } else { rest };
            rest.replace("%20", " ")
        }
        None => v.to_string(),
    };
    let unix = UNIX_ROOTS.iter().any(|r| p == *r || p.strip_prefix(r).is_some_and(|t| t.starts_with('/')));
    (is_drive_path(&p) || unix).then_some(p)
}

fn is_drive_path(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Under the home folder: /tmp isn't one of the roots a path is recognized by.
    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir_in(dirs::home_dir().unwrap()).unwrap()
    }

    fn hits(file: &str, text: &str) -> Vec<(String, String)> {
        find_absolute_paths(file, text).into_iter().map(|h| (h.key, hit_path(file, &h.value).unwrap())).collect()
    }

    fn kv(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn properties_active_lines_only_with_spaces_and_escapes() {
        // The real Artigem shape: one active Windows path with a space, other
        // developers' paths commented out.
        let text = "#company.configuration.filepath=C:\\\\configuration\\\\company_configuration.xml\n\
                    company.configuration.filepath=C:/Users/Sakha Global/Desktop/Projects/Artigem/configuration/company_configuration.xml\n\
                    #company.configuration.filepath=/usr/local/artigem/configuration/company_configuration.xml\n\
                    ! also.commented=/home/x/y\n\
                    escaped.path=C\\:\\\\data\\\\tenants.xml\n\
                    colon.sep : /opt/app/conf\n\
                    mediafiles.server.path=/artigemrs-mediafiles/\n\
                    url=http://localhost:8080/api\n\
                    relative=configuration/x.xml\n\
                    placeholder=${user.home}/x\n";
        assert_eq!(
            hits("src/main/resources/application.properties", text),
            kv(&[
                ("company.configuration.filepath", "C:/Users/Sakha Global/Desktop/Projects/Artigem/configuration/company_configuration.xml"),
                ("escaped.path", "C:\\data\\tenants.xml"),
                ("colon.sep", "/opt/app/conf"),
            ])
        );
        let h = &find_absolute_paths("a.properties", text)[0];
        assert_eq!(h.line, 2);
    }

    #[test]
    fn yaml_nested_keys_quotes_and_comments() {
        let text = "company:\n  configuration:\n    # filepath: /home/old/x.xml\n    filepath: \"C:\\\\Users\\\\Some One\\\\tenants.xml\"\n    other: plain\nlogos: '/srv/logos/' # trailing comment\nlist:\n  - /home/a\n---\nfile: file:///C:/Users/Some%20One/x.xml\n";
        assert_eq!(
            hits("config/application.yml", text),
            kv(&[
                ("company.configuration.filepath", "C:\\Users\\Some One\\tenants.xml"),
                ("logos", "/srv/logos/"),
                ("file", "C:/Users/Some One/x.xml"),
            ])
        );
    }

    #[test]
    fn env_files_with_export_quotes_and_comments() {
        let text = "# TENANTS=/home/me/t.xml\nexport TENANTS=\"/home/dev/my tenants.xml\"\nDATA=/var/data # where\nNAME=thing\n";
        assert_eq!(hits(".env.local", text), kv(&[("TENANTS", "/home/dev/my tenants.xml"), ("DATA", "/var/data")]));
        assert!(hits("notes.txt", text).is_empty(), "other files aren't config");
    }

    #[test]
    fn simple_xml_key_values_outside_comments() {
        let text = "<config>\n<!-- <path>/home/old/x</path> -->\n<!--\n<path>/home/older/y</path>\n-->\n<path>C:/Data Dir/t.xml</path>\n<entry key=\"tenants\">/opt/t.xml</entry>\n<property name=\"logos\" value=\"/srv/logos\"/>\n<url>http://x/y</url>\n</config>\n";
        assert_eq!(hits("conf/app.xml", text), kv(&[("path", "C:/Data Dir/t.xml"), ("tenants", "/opt/t.xml"), ("logos", "/srv/logos")]));
    }

    #[test]
    fn rewrites_replace_only_the_active_value_in_each_syntax() {
        let props = "#k=/home/old\r\nk = C:/Users/Some One/t.xml\r\nother=1\r\n";
        let (from, out) = rewrite_value("a.properties", props, "k", "C:/Users/Some One/t.xml", "/localsync/external/k-1/t.xml").unwrap();
        assert_eq!(from, "C:/Users/Some One/t.xml");
        assert_eq!(out, "#k=/home/old\r\nk = /localsync/external/k-1/t.xml\r\nother=1\r\n");

        let yml = "a:\n  b: \"C:\\\\x y\\\\t.xml\" # c\n";
        let (_, out) = rewrite_value("x.yaml", yml, "a.b", "C:\\\\x y\\\\t.xml", "/localsync/external/a-1/t.xml").unwrap();
        assert_eq!(out, "a:\n  b: \"/localsync/external/a-1/t.xml\" # c\n");

        let env = "export T='/home/a b/t.xml'\n";
        let (_, out) = rewrite_value(".env", env, "T", "/home/a b/t.xml", "/localsync/external/t-1/t.xml").unwrap();
        assert_eq!(out, "export T='/localsync/external/t-1/t.xml'\n");

        assert!(rewrite_value("a.xml", "<k>/home/x</k>", "k", "/home/x", "/y").is_err(), "XML isn't rewritten yet");
        assert!(rewrite_value("a.properties", "k=/a\nk=/b\n", "k", "/c", "/y").is_err(), "ambiguous");
        let (_, out) = rewrite_value("a.properties", "k=/a\nk=/b\n", "k", "/b", "/y").unwrap();
        assert_eq!(out, "k=/a\nk=/y\n");
    }

    #[test]
    fn scan_sorts_inside_outside_and_missing_and_skips_build_folders() {
        let base = tmp();
        let project = base.path().join("proj");
        let outside = base.path().join("outside dir");
        std::fs::create_dir_all(project.join("src/main/resources")).unwrap();
        std::fs::create_dir_all(project.join("target/classes")).unwrap();
        std::fs::create_dir_all(project.join("conf")).unwrap();
        std::fs::create_dir_all(outside.join("logos")).unwrap();
        std::fs::write(outside.join("tenants.xml"), "<companies/>").unwrap();
        std::fs::write(outside.join("logos/a.png"), "12345").unwrap();
        std::fs::write(project.join("conf/inner.xml"), "x").unwrap();
        let abs = |p: &Path| p.to_string_lossy().replace('\\', "/");
        let props = format!(
            "tenants={}\nlogos={}/\ninner={}\nmissing={}\n#old=/home/someone/x.xml\n",
            abs(&outside.join("tenants.xml")),
            abs(&outside.join("logos")),
            abs(&project.join("conf/inner.xml")),
            if cfg!(windows) { "C:/no/such/place.xml" } else { "/home/no-such-user-ls/place.xml" }
        );
        std::fs::write(project.join("src/main/resources/application.properties"), &props).unwrap();
        std::fs::write(project.join("target/classes/application.properties"), &props).unwrap();

        let refs = scan(&project).unwrap();
        assert_eq!(refs.len(), 4, "the target/ copy is skipped: {refs:#?}");
        let by = |k: &str| refs.iter().find(|r| r.key == k).unwrap();
        assert_eq!(by("tenants").status, ExternalStatus::OutsideExists);
        assert_eq!(by("tenants").kind, ExternalKind::File);
        assert_eq!(by("tenants").size_bytes, 12);
        assert_eq!(by("tenants").config_file, "src/main/resources/application.properties");
        assert_eq!(by("tenants").line, 1);
        assert!(by("tenants").container_path.starts_with("/localsync/external/tenants-xml-"));
        assert!(by("tenants").container_path.ends_with("/tenants.xml"));
        assert!(by("tenants").rewritable);
        assert_eq!(by("logos").kind, ExternalKind::Dir);
        assert_eq!(by("logos").size_bytes, 5);
        assert_eq!(by("inner").status, ExternalStatus::Inside);
        assert_eq!(by("inner").suggested_relative.as_deref(), Some("conf/inner.xml"));
        assert_eq!(by("missing").status, ExternalStatus::OutsideMissing);
        assert_eq!(std::fs::read_to_string(project.join("src/main/resources/application.properties")).unwrap(), props, "read-only");
    }

    #[test]
    fn fingerprint_follows_content_and_apply_rewrites_localsyncs_copy_only() {
        let base = tmp();
        let project = base.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let ext = base.path().join("my tenants.xml");
        std::fs::write(&ext, "<a/>").unwrap();
        let config = format!("tenants={0}\nalso.tenants={0}\n", ext.to_string_lossy().replace('\\', "/"));
        std::fs::write(project.join("application.properties"), &config).unwrap();
        let refs = scan(&project).unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].id, refs[1].id, "one file, one id");
        assert_eq!(externals_fingerprint(&[]), None);
        let before = externals_fingerprint(&refs).unwrap();
        assert_eq!(externals_fingerprint(&refs).unwrap(), before, "stable");
        std::fs::write(&ext, "<b/>").unwrap();
        assert_ne!(externals_fingerprint(&refs).unwrap(), before, "a modified external file changes it");

        let mut g = ls_composegen::Generated {
            compose_yaml: "services:\n  app:\n    image: x\n".into(),
            files: vec![],
            rewrites: vec![],
            notes: vec![],
            host_port: 1,
        };
        apply_to_generated(&project, &refs, &mut g).unwrap();
        let id = &refs[0].id;
        assert_eq!(g.compose_yaml.matches(&format!("../external/{id}:/localsync/external/{id}:ro")).count(), 1, "{}", g.compose_yaml);
        assert_eq!(g.files.len(), 1);
        assert_eq!(g.files[0].path, "application.properties");
        assert_eq!(g.files[0].contents, format!("tenants=/localsync/external/{id}/my_tenants.xml\nalso.tenants=/localsync/external/{id}/my_tenants.xml\n"));
        assert_eq!(g.rewrites.len(), 2);
        assert_eq!(std::fs::read_to_string(project.join("application.properties")).unwrap(), config, "the person's file is untouched");
        let bundled = bundle_entries(&refs);
        assert_eq!(bundled[0].0.bundle_path, format!("external/{id}/my_tenants.xml"));
    }

    /// Real Windows paths as people write them: backslashes (escaped in
    /// .properties, raw in .env), a space in the user folder, another letter
    /// case for the drive and folders.
    #[cfg(windows)]
    #[test]
    fn real_windows_paths_resolve_on_this_computer() {
        let base = tmp();
        let project = base.path().join("proj");
        std::fs::create_dir_all(project.join("conf")).unwrap();
        let ext = base.path().join("Some Dir").join("tenants.xml");
        std::fs::create_dir_all(ext.parent().unwrap()).unwrap();
        std::fs::write(&ext, "<companies/>").unwrap();
        std::fs::write(project.join("conf/in.xml"), "x").unwrap();
        let back = ext.to_string_lossy().to_string();
        assert!(back.contains('\\') && back.contains(' '), "{back}");
        let inside_other_case = project.join("conf/in.xml").to_string_lossy().to_lowercase();
        std::fs::write(
            project.join("application.properties"),
            format!("escaped={}\ninner={}\n", back.replace('\\', "\\\\"), inside_other_case.replace('\\', "\\\\")),
        )
        .unwrap();
        std::fs::write(project.join(".env"), format!("RAW={back}\nQUOTED=\"{back}\"\n")).unwrap();
        let refs = scan(&project).unwrap();
        let by = |k: &str| refs.iter().find(|r| r.key == k).unwrap_or_else(|| panic!("{k} not found in {refs:#?}"));
        for k in ["escaped", "RAW", "QUOTED"] {
            assert_eq!(by(k).status, ExternalStatus::OutsideExists, "{k}");
            assert_eq!(by(k).path, back, "{k}");
            assert_eq!(by(k).size_bytes, 12);
        }
        assert_eq!(by("inner").status, ExternalStatus::Inside);
        assert_eq!(by("inner").suggested_relative.as_deref(), Some("conf/in.xml"));
        let (_, rewritten) = rewrite_value("application.properties", &std::fs::read_to_string(project.join("application.properties")).unwrap(), "escaped", &by("escaped").value, &by("escaped").container_path).unwrap();
        assert!(rewritten.starts_with(&format!("escaped={}\n", by("escaped").container_path)), "{rewritten}");
    }

    #[test]
    fn describe_finds_the_key_or_mounts_without_rewriting() {
        let base = tmp();
        let project = base.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("app.properties"), "tenants=../conf/t.xml\n").unwrap();
        let ext = base.path().join("t.xml");
        std::fs::write(&ext, "x").unwrap();
        let p = ext.to_string_lossy().to_string();
        let r = describe(&project, "tenants", &p).unwrap();
        assert_eq!((r.config_file.as_str(), r.value.as_str(), r.status), ("app.properties", "../conf/t.xml", ExternalStatus::OutsideExists));
        let r = describe(&project, "nowhere.key", &p).unwrap();
        assert_eq!(r.config_file, "");
        assert!(!r.rewritable);
        assert!(describe(&project, "k", &base.path().join("nope").to_string_lossy()).is_err());
    }
}

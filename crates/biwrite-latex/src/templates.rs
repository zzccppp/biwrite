//! Paper templates: folders with a `template.json` manifest. Built-in ones
//! ship with the app (read-only), imported ones live in the app's data
//! folder. A new paper is a copy of a template in a folder the user names.

use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::project::{Engine, engine_for, find_main};

pub const MANIFEST: &str = "template.json";

/// Most files and bytes copied from one template, project or archive.
const MAX_FILES: usize = 600;
const MAX_BYTES: u64 = 80 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// The file to compile, relative to the template folder.
    pub main: String,
    #[serde(default)]
    pub engine: Option<Engine>,
    /// Where the template files come from (a URL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    /// The folder name.
    pub id: String,
    pub builtin: bool,
    #[serde(flatten)]
    pub manifest: Manifest,
    pub files: usize,
    pub bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    #[error("no template named {0}")]
    Unknown(String),
    #[error("built-in templates cannot be removed")]
    Builtin,
    #[error("{0} has no main .tex file (one with \\documentclass and \\begin{{document}})")]
    NoMain(String),
    #[error("{0} already exists and is not empty: choose a new folder name")]
    NotEmpty(String),
    #[error("too large: more than {MAX_FILES} files or {} MB", MAX_BYTES / 1024 / 1024)]
    TooLarge,
    #[error("the archive is not a usable .zip: {0}")]
    Zip(String),
    #[error("{path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> TemplateError + '_ {
    move |source| TemplateError::Io {
        path: path.display().to_string(),
        source,
    }
}

/// Build output and editor files that never belong in a template.
const SKIP_EXTENSIONS: &[&str] = &[
    "aux",
    "log",
    "out",
    "toc",
    "lof",
    "lot",
    "fls",
    "fdb_latexmk",
    "synctex",
    "gz",
    "blg",
    "bbl",
    "bcf",
    "nav",
    "snm",
    "vrb",
    "xdv",
    "dvi",
    "idx",
    "ilg",
    "ind",
    "glo",
    "gls",
    "glg",
    "acn",
    "acr",
    "alg",
    "ist",
    "brf",
    "thm",
    "loa",
    "lol",
    "run.xml",
    "biwrite-tmp",
    "zip",
];
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "build",
    "out",
    "dist",
    "history",
    "__pycache__",
    "__MACOSX",
];

/// Source files of a project folder (relative paths), leaving out hidden
/// files, build output and the compiled PDF of `main`.
pub fn project_files(dir: &Path, main: Option<&Path>) -> Result<Vec<PathBuf>, TemplateError> {
    let output = main
        .and_then(Path::file_stem)
        .map(|s| format!("{}.pdf", s.to_string_lossy()));
    let mut out = Vec::new();
    let mut bytes = 0u64;
    walk(
        dir,
        Path::new(""),
        0,
        output.as_deref(),
        &mut out,
        &mut bytes,
    )?;
    out.sort();
    Ok(out)
}

fn walk(
    root: &Path,
    rel: &Path,
    depth: usize,
    output: Option<&str>,
    out: &mut Vec<PathBuf>,
    bytes: &mut u64,
) -> Result<(), TemplateError> {
    if depth > 8 {
        return Ok(());
    }
    let dir = root.join(rel);
    let mut entries: Vec<_> = fs::read_dir(&dir)
        .map_err(io(&dir))?
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == MANIFEST {
            continue;
        }
        let path = rel.join(&name);
        let Ok(kind) = e.file_type() else { continue };
        if kind.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) {
                walk(root, &path, depth + 1, output, out, bytes)?;
            }
            continue;
        }
        if !kind.is_file()
            || SKIP_EXTENSIONS
                .iter()
                .any(|x| name.ends_with(&format!(".{x}")))
            || name.ends_with("-blx.bib")
            || depth == 0 && output == Some(name.as_str())
        {
            continue;
        }
        *bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
        out.push(path);
        if out.len() > MAX_FILES || *bytes > MAX_BYTES {
            return Err(TemplateError::TooLarge);
        }
    }
    Ok(())
}

fn read_manifest(dir: &Path) -> Option<Manifest> {
    let text = fs::read_to_string(dir.join(MANIFEST)).ok()?;
    let m: Manifest = serde_json::from_str(&text).ok()?;
    (!m.name.trim().is_empty() && safe_relative(Path::new(&m.main))).then_some(m)
}

fn describe(dir: &Path, builtin: bool) -> Option<Template> {
    let manifest = read_manifest(dir)?;
    let files = project_files(dir, None).ok()?;
    let bytes = files
        .iter()
        .filter_map(|f| fs::metadata(dir.join(f)).ok())
        .map(|m| m.len())
        .sum();
    Some(Template {
        id: dir.file_name()?.to_string_lossy().into_owned(),
        builtin,
        manifest,
        files: files.len(),
        bytes,
    })
}

/// Built-in templates first (in folder order), then the user's by name.
pub fn list(builtin: &Path, user: &Path) -> Vec<Template> {
    let mut out = Vec::new();
    for (root, is_builtin) in [(builtin, true), (user, false)] {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        let mut found: Vec<Template> = entries
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir() && !e.file_name().to_string_lossy().starts_with('.'))
            .filter_map(|e| describe(&e.path(), is_builtin))
            .collect();
        if is_builtin {
            found.sort_by(|a, b| a.id.cmp(&b.id));
        } else {
            found.sort_by_key(|t| t.manifest.name.to_lowercase());
        }
        out.extend(found);
    }
    out
}

/// The template folder for `id`.
pub fn find(builtin: &Path, user: &Path, id: &str) -> Result<(PathBuf, Template), TemplateError> {
    if !safe_id(id) {
        return Err(TemplateError::Unknown(id.to_owned()));
    }
    for (root, is_builtin) in [(builtin, true), (user, false)] {
        let dir = root.join(id);
        if let Some(t) = describe(&dir, is_builtin) {
            return Ok((dir, t));
        }
    }
    Err(TemplateError::Unknown(id.to_owned()))
}

/// Copy a template into `dest` (created; it must not hold files yet).
/// Returns the main file to open.
pub fn instantiate(template: &Path, dest: &Path) -> Result<PathBuf, TemplateError> {
    let manifest = read_manifest(template)
        .ok_or_else(|| TemplateError::Unknown(template.display().to_string()))?;
    if fs::read_dir(dest).is_ok_and(|mut d| d.next().is_some()) {
        return Err(TemplateError::NotEmpty(dest.display().to_string()));
    }
    for rel in project_files(template, None)? {
        let to = dest.join(&rel);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(io(parent))?;
        }
        fs::copy(template.join(&rel), &to).map_err(io(&to))?;
    }
    Ok(dest.join(manifest.main))
}

/// Copy a project folder into the user's templates. `name` defaults to the
/// folder's name.
pub fn import_folder(
    src: &Path,
    user: &Path,
    name: Option<&str>,
) -> Result<Template, TemplateError> {
    let main = find_main(src).ok_or_else(|| TemplateError::NoMain(src.display().to_string()))?;
    let files = project_files(src, Some(&main))?;
    let name = name
        .map(str::to_owned)
        .or_else(|| src.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "Template".into());
    let dest = unique_dir(user, &slug(&name))?;
    let result = (|| {
        for rel in &files {
            let to = dest.join(rel);
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent).map_err(io(parent))?;
            }
            fs::copy(src.join(rel), &to).map_err(io(&to))?;
        }
        let text = fs::read_to_string(&main).unwrap_or_default();
        let manifest = Manifest {
            name: name.clone(),
            description: String::new(),
            main: relative(&main, src),
            engine: Some(engine_for(&text)),
            source: None,
        };
        write_manifest(&dest, &manifest)?;
        describe(&dest, false).ok_or_else(|| TemplateError::NoMain(name.clone()))
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&dest);
    }
    result
}

/// Import a `.zip` (a template exported by BiWrite, or any LaTeX project).
pub fn import_zip(archive: &Path, user: &Path) -> Result<Template, TemplateError> {
    let name = archive
        .file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Template".into());
    fs::create_dir_all(user).map_err(io(user))?;
    let staging = unique_dir(user, ".import")?;
    let result = (|| {
        extract(archive, &staging)?;
        // A single top folder holds the project.
        let mut top: Vec<PathBuf> = fs::read_dir(&staging)
            .map_err(io(&staging))?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| !n.to_string_lossy().starts_with('.') && n != "__MACOSX")
            })
            .collect();
        let root = if top.len() == 1 && top[0].is_dir() {
            top.remove(0)
        } else {
            staging.clone()
        };
        let manifest = read_manifest(&root);
        let mut t = import_folder(
            &root,
            user,
            Some(manifest.as_ref().map_or(name.as_str(), |m| m.name.as_str())),
        )?;
        if let Some(m) = manifest {
            // Keep the exported description and engine.
            let dest = user.join(&t.id);
            let main = if dest.join(&m.main).is_file() {
                m.main
            } else {
                t.manifest.main.clone()
            };
            t.manifest = Manifest { main, ..m };
            write_manifest(&dest, &t.manifest)?;
        }
        Ok(t)
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}

/// Zip a project folder as a template (with a manifest naming `main`).
/// Returns the number of files written.
pub fn export_zip(dir: &Path, main: &Path, dest: &Path) -> Result<usize, TemplateError> {
    let files = project_files(dir, Some(main))?;
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "paper".into());
    let manifest = Manifest {
        name: name.clone(),
        description: String::new(),
        main: relative(main, dir),
        engine: Some(engine_for(&fs::read_to_string(main).unwrap_or_default())),
        source: None,
    };
    let tmp = dest.with_extension("zip.biwrite-tmp");
    let result = (|| {
        let file = fs::File::create(&tmp).map_err(io(&tmp))?;
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let prefix = slug(&name);
        let zip_err = |e: zip::result::ZipError| TemplateError::Zip(e.to_string());
        zip.start_file(format!("{prefix}/{MANIFEST}"), options)
            .map_err(zip_err)?;
        let json = serde_json::to_vec_pretty(&manifest).unwrap_or_default();
        zip.write_all(&json).map_err(io(dest))?;
        for rel in &files {
            let unix: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            zip.start_file(format!("{prefix}/{}", unix.join("/")), options)
                .map_err(zip_err)?;
            let bytes = fs::read(dir.join(rel)).map_err(io(&dir.join(rel)))?;
            zip.write_all(&bytes).map_err(io(dest))?;
        }
        zip.finish().map_err(zip_err)?;
        fs::rename(&tmp, dest).map_err(io(dest))?;
        Ok(files.len())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Remove an imported template.
pub fn delete(builtin: &Path, user: &Path, id: &str) -> Result<(), TemplateError> {
    let (dir, t) = find(builtin, user, id)?;
    if t.builtin {
        return Err(TemplateError::Builtin);
    }
    fs::remove_dir_all(&dir).map_err(io(&dir))
}

/// Unpack `archive` into `dest`, refusing entries that would land outside
/// it and archives over the size limits.
fn extract(archive: &Path, dest: &Path) -> Result<(), TemplateError> {
    let file = fs::File::open(archive).map_err(io(archive))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| TemplateError::Zip(e.to_string()))?;
    if zip.len() > MAX_FILES * 2 {
        return Err(TemplateError::TooLarge);
    }
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| TemplateError::Zip(e.to_string()))?;
        let Some(rel) = entry.enclosed_name() else {
            return Err(TemplateError::Zip(format!(
                "unsafe path {}",
                entry.name().unwrap_or_default()
            )));
        };
        let to = dest.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&to).map_err(io(&to))?;
            continue;
        }
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(io(parent))?;
        }
        let mut bytes = Vec::new();
        // Read one byte past the remaining budget to detect oversized entries.
        let budget = MAX_BYTES.saturating_sub(total) + 1;
        (&mut entry)
            .take(budget)
            .read_to_end(&mut bytes)
            .map_err(|e| TemplateError::Zip(e.to_string()))?;
        total += bytes.len() as u64;
        if total > MAX_BYTES {
            return Err(TemplateError::TooLarge);
        }
        fs::write(&to, &bytes).map_err(io(&to))?;
    }
    Ok(())
}

fn write_manifest(dir: &Path, m: &Manifest) -> Result<(), TemplateError> {
    let path = dir.join(MANIFEST);
    let json = serde_json::to_vec_pretty(m).unwrap_or_default();
    fs::write(&path, json).map_err(io(&path))
}

/// `main` relative to `dir`, with `/` separators.
fn relative(main: &Path, dir: &Path) -> String {
    let rel = main.strip_prefix(dir).unwrap_or(main);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty() && path.components().all(|c| matches!(c, Component::Normal(_)))
}

fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('.')
        && safe_relative(Path::new(id))
        && !id.contains(['/', '\\'])
}

/// A folder name from a template name: letters, digits and dashes.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out
        .trim_end_matches('-')
        .chars()
        .take(40)
        .collect::<String>();
    if out.is_empty() {
        "template".into()
    } else {
        out
    }
}

/// A new folder under `root` named `base`, `base-2`, `base-3`, …
fn unique_dir(root: &Path, base: &str) -> Result<PathBuf, TemplateError> {
    fs::create_dir_all(root).map_err(io(root))?;
    for n in 1..1000 {
        let name = if n == 1 {
            base.to_owned()
        } else {
            format!("{base}-{n}")
        };
        let dir = root.join(name);
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io(&dir)(e)),
        }
    }
    Err(TemplateError::TooLarge)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("biwrite-tpl-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn project(dir: &Path) {
        fs::create_dir_all(dir.join("figures")).unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(
            dir.join("paper.tex"),
            "\\documentclass{article}\n\\usepackage{fontspec}\n\\begin{document}\nHi\n\\end{document}\n",
        )
        .unwrap();
        fs::write(dir.join("refs.bib"), "@misc{a,title={A}}").unwrap();
        fs::write(dir.join("figures/plot.pdf"), "%PDF-1.5").unwrap();
        fs::write(dir.join("paper.pdf"), "%PDF-1.5 output").unwrap();
        fs::write(dir.join("paper.aux"), "\\relax").unwrap();
        fs::write(dir.join(".git/config"), "[core]").unwrap();
    }

    #[test]
    fn project_files_leave_out_build_output() {
        let dir = temp("files");
        project(&dir);
        let files = project_files(&dir, Some(&dir.join("paper.tex"))).unwrap();
        let names: Vec<String> = files.iter().map(|f| f.display().to_string()).collect();
        assert_eq!(
            names,
            ["figures/plot.pdf", "paper.tex", "refs.bib"]
                .map(|s| s.replace('/', std::path::MAIN_SEPARATOR_STR))
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn import_export_and_instantiate_round_trip() {
        let root = temp("roundtrip");
        let (builtin, user, src) = (
            root.join("builtin"),
            root.join("user"),
            root.join("My Paper"),
        );
        fs::create_dir_all(&builtin).unwrap();
        project(&src);

        // Folder import names the template after the folder.
        let t = import_folder(&src, &user, None).unwrap();
        assert_eq!(t.id, "my-paper");
        assert_eq!(t.manifest.main, "paper.tex");
        assert_eq!(t.manifest.engine, Some(Engine::Xelatex));
        assert_eq!(t.files, 3);

        // Export, then import the archive: a second copy with its own id.
        let zip_path = root.join("export.zip");
        assert_eq!(
            export_zip(&src, &src.join("paper.tex"), &zip_path).unwrap(),
            3
        );
        let again = import_zip(&zip_path, &user).unwrap();
        assert_eq!(again.id, "my-paper-2");
        assert_eq!(again.manifest.name, "My Paper");
        assert_eq!(list(&builtin, &user).len(), 2);

        // A new paper from the template; an existing non-empty folder is refused.
        let (dir, _) = find(&builtin, &user, "my-paper").unwrap();
        let dest = root.join("new paper");
        let main = instantiate(&dir, &dest).unwrap();
        assert_eq!(main, dest.join("paper.tex"));
        assert!(dest.join("figures/plot.pdf").is_file());
        assert!(!dest.join(MANIFEST).exists());
        assert!(matches!(
            instantiate(&dir, &dest),
            Err(TemplateError::NotEmpty(_))
        ));

        delete(&builtin, &user, "my-paper-2").unwrap();
        assert!(matches!(
            find(&builtin, &user, "../user"),
            Err(TemplateError::Unknown(_))
        ));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn zip_entries_outside_the_folder_are_refused() {
        let root = temp("zipslip");
        let zip_path = root.join("evil.zip");
        {
            let file = fs::File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            zip.start_file("../escape.tex", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"x").unwrap();
            zip.finish().unwrap();
        }
        assert!(matches!(
            import_zip(&zip_path, &root.join("user")),
            Err(TemplateError::Zip(_))
        ));
        assert!(!root.join("escape.tex").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("ICLR 2027 (conference)"), "iclr-2027-conference");
        assert_eq!(slug("中文 模板"), "中文-模板");
        assert_eq!(slug("!!!"), "template");
    }
}

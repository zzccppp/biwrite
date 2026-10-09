//! Finding the TeX tools. Apps started from the Dock or the Start menu get a
//! minimal `PATH`, so the usual install folders are searched as well.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::project::Engine;

/// The TeX tools found on this computer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Toolchain {
    /// Folder holding the engines.
    pub bin: PathBuf,
    /// `TeX Live 2023`, `MiKTeX 24.1`, … (empty when the version line has none).
    pub distribution: String,
    pub latexmk: bool,
    pub synctex: bool,
    /// Engines present in `bin`.
    pub engines: Vec<Engine>,
}

impl Toolchain {
    pub fn tool(&self, name: &str) -> PathBuf {
        self.bin.join(exe(name))
    }

    pub fn has(&self, engine: Engine) -> bool {
        self.engines.contains(&engine)
    }

    /// `PATH` for child processes: `bin` first, so latexmk finds the
    /// engines, BibTeX and Biber next to it.
    pub fn path_env(&self) -> OsString {
        let mut paths = vec![self.bin.clone()];
        if let Some(path) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&path).filter(|p| p != &self.bin));
        }
        std::env::join_paths(paths).unwrap_or_default()
    }
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

/// Folders where TeX distributions put their programs, most likely first.
fn candidates(home: Option<&Path>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        out.extend(std::env::split_paths(&path));
    }
    let texlive = |root: &Path, out: &mut Vec<PathBuf>| {
        // Newest year first: /usr/local/texlive/2025/bin/<platform>.
        let Ok(entries) = std::fs::read_dir(root) else {
            return;
        };
        let mut years: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.len() == 4 && n.bytes().all(|b| b.is_ascii_digit()))
            })
            .collect();
        years.sort();
        for year in years.iter().rev() {
            if let Ok(platforms) = std::fs::read_dir(year.join("bin")) {
                out.extend(platforms.filter_map(Result::ok).map(|e| e.path()));
            }
        }
    };
    if cfg!(target_os = "macos") {
        out.push("/Library/TeX/texbin".into());
        texlive(Path::new("/usr/local/texlive"), &mut out);
        out.push("/opt/homebrew/bin".into());
        out.push("/usr/local/bin".into());
    } else if cfg!(windows) {
        texlive(Path::new("C:\\texlive"), &mut out);
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            out.push(Path::new(&local).join("Programs\\MiKTeX\\miktex\\bin\\x64"));
        }
        for var in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(pf) = std::env::var_os(var) {
                out.push(Path::new(&pf).join("MiKTeX\\miktex\\bin\\x64"));
                out.push(Path::new(&pf).join("MiKTeX\\miktex\\bin"));
            }
        }
    } else {
        texlive(Path::new("/usr/local/texlive"), &mut out);
        if let Some(home) = home {
            texlive(&home.join("texlive"), &mut out);
        }
        out.push("/usr/bin".into());
        out.push("/usr/local/bin".into());
    }
    out
}

/// The first folder with a TeX engine: `preferred` (a folder the user
/// chose), then `PATH`, then the usual install folders.
pub fn find_bin(preferred: Option<&Path>) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut folders: Vec<PathBuf> = preferred.map(Path::to_path_buf).into_iter().collect();
    folders.extend(candidates(home.as_deref()));
    folders.into_iter().find(|dir| {
        ["pdflatex", "xelatex", "lualatex"]
            .iter()
            .any(|e| dir.join(exe(e)).is_file())
    })
}

/// Detect the toolchain in `bin` (see [`find_bin`]). Runs `--version` once,
/// with a short timeout.
pub async fn detect(bin: PathBuf) -> Toolchain {
    let engines: Vec<Engine> = [Engine::Pdflatex, Engine::Xelatex, Engine::Lualatex]
        .into_iter()
        .filter(|e| bin.join(exe(e.as_str())).is_file())
        .collect();
    let probe = engines.first().map_or(Engine::Pdflatex, |e| *e);
    let distribution = version_line(&bin.join(exe(probe.as_str())))
        .await
        .map(|line| distribution_of(&line))
        .unwrap_or_default();
    Toolchain {
        latexmk: bin.join(exe("latexmk")).is_file(),
        synctex: bin.join(exe("synctex")).is_file(),
        distribution,
        engines,
        bin,
    }
}

async fn version_line(program: &Path) -> Option<String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.arg("--version")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(10), cmd.output())
        .await
        .ok()?
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(str::to_owned)
}

/// `pdfTeX 3.14… (TeX Live 2023)` → `TeX Live 2023`;
/// `MiKTeX-pdfTeX 4.16 (MiKTeX 24.1)` → `MiKTeX 24.1`.
fn distribution_of(line: &str) -> String {
    match (line.rfind('('), line.rfind(')')) {
        (Some(a), Some(b)) if b > a => line[a + 1..b].trim().to_owned(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distribution_names() {
        assert_eq!(
            distribution_of("pdfTeX 3.141592653-2.6-1.40.25 (TeX Live 2023)"),
            "TeX Live 2023"
        );
        assert_eq!(
            distribution_of("MiKTeX-pdfTeX 4.16 (MiKTeX 24.1)"),
            "MiKTeX 24.1"
        );
        assert_eq!(distribution_of("pdfTeX 3.14"), "");
    }

    #[test]
    fn a_chosen_folder_without_engines_is_skipped() {
        let empty = std::env::temp_dir().join(format!("biwrite-texbin-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        let found = find_bin(Some(&empty));
        assert_ne!(found.as_deref(), Some(empty.as_path()));
        std::fs::remove_dir_all(&empty).unwrap();
    }
}

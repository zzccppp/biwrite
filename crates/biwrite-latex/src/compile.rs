//! Compiling a project: latexmk when it is installed (it reruns the engine
//! and BibTeX or Biber as often as needed), otherwise the engine directly.
//!
//! Every tool runs in its own process group, which is killed when the
//! compile future is dropped or times out, so cancelling a build leaves no
//! engine running.

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

use crate::log::{self, Issue, Severity};
use crate::project::{Engine, tex_files};
use crate::toolchain::Toolchain;

/// One build.
#[derive(Clone, Debug)]
pub struct Job {
    /// The folder TeX runs in: relative paths in the document resolve here.
    pub dir: PathBuf,
    /// The root `.tex` file, relative to `dir`.
    pub root: PathBuf,
    pub engine: Engine,
    /// Output folder relative to `dir`; `None` writes into `dir`, next to
    /// the root, where the PDF is expected.
    pub out_dir: Option<PathBuf>,
    pub timeout: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// A PDF and no errors.
    Ok,
    /// A PDF, but the log has errors.
    Errors,
    /// No PDF.
    Failed,
    TimedOut,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Compiled {
    pub outcome: Outcome,
    pub pdf: Option<PathBuf>,
    /// The PDF predates this build (the build stopped before writing one).
    pub stale: bool,
    pub issues: Vec<Issue>,
    pub duration_ms: u64,
    /// The command that ran, for the status line.
    pub tool: String,
    /// The end of the tools' console output, shown when there is no log.
    pub output: String,
    /// The project's own latexmk configuration (`latexmkrc` or
    /// `.latexmkrc` in its folder), which latexmk ran as Perl: it can run
    /// any command, so the user is told.
    pub project_rc: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("{0} is not installed")]
    NoEngine(&'static str),
    #[error("{0} does not exist")]
    NoRoot(String),
    #[error(
        "{0} can't be compiled: rename it to letters, digits, spaces and . _ - + , ( ) [ ] @ = ' \
         only, not starting with '-' (other characters can run commands)"
    )]
    UnsafeName(String),
    #[error("could not start {tool}: {source}")]
    Spawn {
        tool: String,
        source: std::io::Error,
    },
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// Console output kept per tool run.
const OUTPUT_TAIL: usize = 24_000;

/// Runs of the engine without latexmk, bibliography pass included.
const MAX_DIRECT_RUNS: usize = 4;

pub async fn compile(tc: &Toolchain, job: &Job) -> Result<Compiled, CompileError> {
    let started = Instant::now();
    let since = SystemTime::now();
    let dir = job.dir.clone();
    if !dir.join(&job.root).is_file() {
        return Err(CompileError::NoRoot(job.root.display().to_string()));
    }
    // The root comes from the project (`% !TEX root = …`, a file name), so
    // a project could name a file `-shell-escape` and have it taken for
    // that option.
    if unsafe_name(&job.root) {
        return Err(CompileError::UnsafeName(job.root.display().to_string()));
    }
    if !tc.has(job.engine) {
        return Err(CompileError::NoEngine(job.engine.as_str()));
    }
    let file = job.root.display().to_string();
    let stem = job
        .root
        .file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let out = dir.join(job.out_dir.as_deref().unwrap_or(Path::new("")));
    if job.out_dir.is_some() {
        prepare_out_dir(&dir, &out)?;
    }
    let log_path = out.join(format!("{stem}.log"));
    let deadline = started + job.timeout;
    // The PDF already there (the author's own, or the last build's) is
    // kept aside, so a build that cannot finish writing a PDF leaves it as
    // it was instead of a broken one.
    let pdf_path = out.join(format!("{stem}.pdf"));
    let kept = keep_pdf(&pdf_path);

    // latexmk goes by the sources alone. A PDF built elsewhere without
    // SyncTeX looks up to date and would stay without it, and so does a PDF
    // a failed run left unfinished (latexmk only remembers that the run
    // failed).
    // A failed run that wrote no PDF is not run again either while the
    // sources stay as they were.
    let unfinished = !pdf_path.is_file() || !complete_pdf(&pdf_path);
    let mut build = Build {
        dir: &dir,
        out: &out,
        file: &file,
        stem: &stem,
        log_path: &log_path,
        deadline,
        pretex: None,
        force: synctex_behind(&out, &stem) || unfinished,
    };
    // CJK text the preamble has no support for gets it from outside.
    if crate::cjk::needs_support(&dir, &job.root) {
        build.pretex = cjk_support(tc, job.engine).await;
        // latexmk does not see the support added to its command, so a last
        // run without it (its log does not load the package) counts as out
        // of date.
        let package = crate::cjk::package_file(job.engine);
        if build.pretex.is_some()
            && !std::fs::read(&log_path)
                .is_ok_and(|log| String::from_utf8_lossy(&log).contains(package))
        {
            build.force = true;
        }
    }
    let mut pass = build.run(tc, job).await?;
    // CJK text the scan could not find (in a file included through a
    // macro) shows in the log, and the build runs again with the support.
    if build.pretex.is_none()
        && !pass.timed_out
        && fresh(&log_path, since)
        && std::fs::read(&log_path)
            .is_ok_and(|log| crate::cjk::missing_in_log(&String::from_utf8_lossy(&log)))
        && let Some(code) = cjk_support(tc, job.engine).await
    {
        build.pretex = Some(code);
        build.force = true;
        pass = build.run(tc, job).await?;
    }
    // When the support itself stopped TeX before any page (a font the
    // system cannot load), the document is built as it stands, as before.
    if build.pretex.is_some()
        && !pass.timed_out
        && fresh(&log_path, since)
        && std::fs::read(&log_path)
            .is_ok_and(|log| String::from_utf8_lossy(&log).contains("no output PDF file produced"))
    {
        build.pretex = None;
        build.force = true;
        pass = build.run(tc, job).await?;
    }
    let Pass {
        tool,
        output,
        timed_out,
        exit_ok,
        project_rc,
    } = pass;

    let mut issues = match std::fs::read(&log_path) {
        Ok(bytes) if fresh(&log_path, since) || !timed_out => {
            log::parse(&String::from_utf8_lossy(&bytes))
        }
        _ => Vec::new(),
    };
    let restored = !complete_pdf(&pdf_path)
        && kept
            .as_deref()
            .is_some_and(|k| std::fs::copy(k, &pdf_path).is_ok());
    if let Some(k) = &kept {
        let _ = std::fs::remove_file(k);
    }
    let pdf = complete_pdf(&pdf_path).then_some(pdf_path);
    let stale = restored || pdf.as_deref().is_some_and(|p| !fresh(p, since));
    let errors = issues.iter().any(|i| i.severity == Severity::Error);
    let outcome = if timed_out {
        Outcome::TimedOut
    } else if pdf.is_none() {
        Outcome::Failed
    } else if errors || !exit_ok {
        Outcome::Errors
    } else {
        Outcome::Ok
    };
    if outcome == Outcome::Errors && !errors {
        // The tools failed without an error in the log (BibTeX, Biber).
        issues.insert(0, tool_error(&output));
    }
    if restored {
        issues.insert(
            0,
            Issue {
                severity: Severity::Error,
                file: None,
                line: None,
                message: "The build could not finish writing the PDF, so the previous PDF is kept \
                          unchanged. The output names the cause."
                    .to_owned(),
            },
        );
    }
    if pdf.is_some() && synctex_of(&out, &stem).is_none() {
        issues.insert(
            0,
            Issue {
                severity: Severity::Warning,
                file: None,
                line: None,
                message: "No SyncTeX data was written for this PDF, so clicks in it cannot find \
                          their place in the source."
                    .to_owned(),
            },
        );
    }
    Ok(Compiled {
        outcome,
        pdf,
        stale,
        issues,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        tool,
        output,
        project_rc,
    })
}

/// One run of the tools over a [`Job`].
struct Build<'a> {
    dir: &'a Path,
    out: &'a Path,
    file: &'a str,
    stem: &'a str,
    log_path: &'a Path,
    deadline: Instant,
    /// TeX code run before the root file is read ([`crate::cjk::pretex`]).
    pretex: Option<String>,
    /// Build even if latexmk takes the PDF for up to date.
    force: bool,
}

/// What a [`Build`] ran and how it ended.
struct Pass {
    tool: String,
    output: String,
    timed_out: bool,
    exit_ok: bool,
    project_rc: Option<String>,
}

impl Build<'_> {
    async fn run(&self, tc: &Toolchain, job: &Job) -> Result<Pass, CompileError> {
        let mut pass = Pass {
            tool: String::new(),
            output: String::new(),
            timed_out: false,
            exit_ok: true,
            project_rc: None,
        };
        let with = if self.pretex.is_some() { " + CJK" } else { "" };
        if tc.latexmk {
            pass.project_rc = ["latexmkrc", ".latexmkrc"]
                .into_iter()
                .find(|name| self.dir.join(name).is_file())
                .map(str::to_owned);
            let mut args = vec![
                job.engine.latexmk_flag().to_owned(),
                // Go on past errors: XeLaTeX writes no PDF otherwise
                // (latexmk stops before xdvipdfmx), and a PDF with the
                // errors listed beats none.
                "-f".to_owned(),
                "-synctex=1".to_owned(),
                "-interaction=nonstopmode".to_owned(),
                "-file-line-error".to_owned(),
            ];
            if let Some(rel) = &job.out_dir {
                args.push(format!("-outdir={}", rel.display()));
            }
            if let Some(code) = &self.pretex {
                args.push(format!("-usepretex={code}"));
            }
            if self.force {
                args.push("-g".to_owned());
            }
            args.push(self.file.to_owned());
            pass.tool = format!("latexmk {}{with}", job.engine.latexmk_flag());
            let run = run(tc, &tc.tool("latexmk"), &args, self.dir, self.deadline).await?;
            pass.output = run.output;
            pass.timed_out = run.timed_out;
            pass.exit_ok = run.success;
        }
        // latexmk missing, or unable to start the engine (MiKTeX without
        // Perl).
        if !pass.timed_out && (!tc.latexmk || !self.log_path.is_file() && !pass.exit_ok) {
            pass.tool = format!("{}{with}", job.engine.as_str());
            let direct = direct(tc, job, self).await?;
            pass.output = direct.output;
            pass.timed_out = direct.timed_out;
            pass.exit_ok = direct.success;
        }
        Ok(pass)
    }
}

/// [`crate::cjk::pretex`] for `engine`, when the TeX distribution has the
/// package it loads. Without it the build goes on as before.
async fn cjk_support(tc: &Toolchain, engine: Engine) -> Option<String> {
    let file = crate::cjk::package_file(engine);
    has_file(tc, file).await.then(|| crate::cjk::pretex(engine))
}

/// Whether `kpsewhich` finds `name` in the TeX distribution. Found files
/// are remembered for the rest of the session.
async fn has_file(tc: &Toolchain, name: &str) -> bool {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock, PoisonError};
    static FOUND: OnceLock<Mutex<HashSet<(PathBuf, String)>>> = OnceLock::new();
    let found = FOUND.get_or_init(Default::default);
    let key = (tc.bin.clone(), name.to_owned());
    if found
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains(&key)
    {
        return true;
    }
    let dir = std::env::temp_dir();
    let mut cmd = command(tc, &tc.tool("kpsewhich"), &[name.to_owned()], &dir);
    cmd.kill_on_drop(true);
    let answer = tokio::time::timeout(Duration::from_secs(10), cmd.output()).await;
    let ok = matches!(answer, Ok(Ok(out))
        if out.status.success() && !String::from_utf8_lossy(&out.stdout).trim().is_empty());
    if ok {
        found
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key);
    }
    ok
}

/// TeX writes `\include`d files' `.aux` next to the main one, so the
/// output folder needs the project's subfolders.
fn prepare_out_dir(dir: &Path, out: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(out)?;
    for file in tex_files(dir) {
        if let Some(parent) = file.parent().and_then(|p| p.strip_prefix(dir).ok())
            && !parent.as_os_str().is_empty()
            && !out.starts_with(dir.join(parent))
        {
            std::fs::create_dir_all(out.join(parent))?;
        }
    }
    Ok(())
}

/// A PDF written to its end. One whose writing stopped (xdvipdfmx giving up
/// on an image, a build stopped) has no `%%EOF` at its end.
fn complete_pdf(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len < 64 {
        return false;
    }
    let tail = len.min(2048);
    let mut end = Vec::new();
    file.seek(SeekFrom::End(-(tail as i64))).is_ok()
        && file.take(tail).read_to_end(&mut end).is_ok()
        && end.windows(5).any(|w| w == b"%%EOF")
}

/// Copy the complete PDF at `pdf` aside, out of the project, for the
/// length of a build. `None` when there is none to keep.
fn keep_pdf(pdf: &Path) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    if !complete_pdf(pdf) {
        return None;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    pdf.hash(&mut hasher);
    let dir = std::env::temp_dir().join("biwrite-kept-pdfs");
    std::fs::create_dir_all(&dir).ok()?;
    let copy = dir.join(format!(
        "{:016x}-{}.pdf",
        hasher.finish(),
        std::process::id()
    ));
    std::fs::copy(pdf, &copy).ok()?;
    Some(copy)
}

/// How much older SyncTeX data may be than its PDF: XeLaTeX writes it
/// before `xdvipdfmx` writes the PDF.
const SYNCTEX_SLACK: Duration = Duration::from_secs(300);

/// The SyncTeX file of the PDF `stem` in `out`, and when it was written.
fn synctex_of(out: &Path, stem: &str) -> Option<SystemTime> {
    [".synctex.gz", ".synctex"].iter().find_map(|ext| {
        std::fs::metadata(out.join(format!("{stem}{ext}")))
            .and_then(|m| m.modified())
            .ok()
    })
}

/// The PDF `stem` in `out` exists without SyncTeX data of its own: none,
/// or written well before the PDF (which was built again elsewhere since).
fn synctex_behind(out: &Path, stem: &str) -> bool {
    let Ok(pdf) = std::fs::metadata(out.join(format!("{stem}.pdf"))).and_then(|m| m.modified())
    else {
        return false;
    };
    match synctex_of(out, stem) {
        None => true,
        Some(synctex) => pdf
            .duration_since(synctex)
            .is_ok_and(|behind| behind > SYNCTEX_SLACK),
    }
}

fn fresh(path: &Path, since: SystemTime) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t >= since)
}

/// A failure that left no error in the TeX log: the last console lines
/// that look like one.
fn tool_error(output: &str) -> Issue {
    let line = output
        .lines()
        .rev()
        .find(|l| {
            let l = l.to_ascii_lowercase();
            l.contains("error") || l.contains("failed") || l.contains("not found")
        })
        .unwrap_or("The tools stopped with an error; see the console output.");
    Issue {
        severity: Severity::Error,
        file: None,
        line: None,
        message: line.trim().to_owned(),
    }
}

struct Run {
    success: bool,
    timed_out: bool,
    output: String,
}

/// The engine directly: once, the bibliography tool when the document has
/// one, then again until the log stops asking for a rerun.
async fn direct(tc: &Toolchain, job: &Job, build: &Build<'_>) -> Result<Run, CompileError> {
    let (dir, out, file, stem, deadline) =
        (build.dir, build.out, build.file, build.stem, build.deadline);
    let mut engine_args = vec![
        "-synctex=1".to_owned(),
        "-interaction=nonstopmode".to_owned(),
        "-file-line-error".to_owned(),
    ];
    if let Some(rel) = &job.out_dir {
        engine_args.push(format!("-output-directory={}", rel.display()));
    }
    match &build.pretex {
        // The job keeps the root's name, though the code comes first.
        Some(code) => {
            engine_args.push(format!("-jobname={stem}"));
            engine_args.push(format!("{code}\\input{{{file}}}"));
        }
        None => engine_args.push(file.to_owned()),
    }
    let engine = tc.tool(job.engine.as_str());
    let mut last = run(tc, &engine, &engine_args, dir, deadline).await?;
    let mut output = last.output.clone();
    let mut runs = 1;
    let aux = std::fs::read_to_string(out.join(format!("{stem}.aux"))).unwrap_or_default();
    let base = job.out_dir.as_deref().map_or_else(
        || stem.to_owned(),
        |rel| rel.join(stem).display().to_string(),
    );
    if !last.timed_out && out.join(format!("{stem}.bcf")).is_file() {
        let args = match &job.out_dir {
            Some(rel) => vec![
                format!("--input-directory={}", rel.display()),
                format!("--output-directory={}", rel.display()),
                stem.to_owned(),
            ],
            None => vec![stem.to_owned()],
        };
        let bib = run(tc, &tc.tool("biber"), &args, dir, deadline).await?;
        output.push_str(&bib.output);
        last.timed_out = bib.timed_out;
    } else if !last.timed_out && aux.contains("\\bibdata") {
        let bib = run(tc, &tc.tool("bibtex"), &[base], dir, deadline).await?;
        output.push_str(&bib.output);
        last.timed_out = bib.timed_out;
    }
    while !last.timed_out && runs < MAX_DIRECT_RUNS {
        let log = std::fs::read_to_string(out.join(format!("{stem}.log"))).unwrap_or_default();
        let bibliography = runs == 1 && (aux.contains("\\bibdata") || aux.contains("\\abx@aux"));
        if !bibliography && !wants_rerun(&log) {
            break;
        }
        last = run(tc, &engine, &engine_args, dir, deadline).await?;
        output.push_str(&last.output);
        runs += 1;
    }
    Ok(Run {
        success: last.success,
        timed_out: last.timed_out,
        output: tail(&output),
    })
}

/// A root a tool could take for something other than a file name. TeX reads
/// a leading `-` as an option, `&` as a format and `\` as code, and latexmk
/// runs the engine through the shell with the name in double quotes, where
/// `` ` `` and `$` run commands. So each part of the path, and the stem
/// (which goes to BibTeX and Biber on its own), may hold only letters,
/// digits and a few plain marks, and may not start with `-`.
fn unsafe_name(root: &Path) -> bool {
    let plain = |part: &str| {
        !part.is_empty()
            && !part.starts_with('-')
            && part
                .chars()
                .all(|c| c.is_alphanumeric() || " ._-+,()[]@='".contains(c))
    };
    let parts = root.components().all(|c| match c {
        Component::Normal(part) => part.to_str().is_some_and(plain),
        Component::CurDir => true,
        _ => false,
    });
    let stem = root.file_stem().and_then(|s| s.to_str()).is_some_and(plain);
    !(parts && stem)
}

fn wants_rerun(log: &str) -> bool {
    log.contains("Rerun to get")
        || log.contains("Label(s) may have changed")
        || log.contains("Please rerun LaTeX")
        || log.contains("Please (re)run Biber")
}

fn tail(text: &str) -> String {
    if text.len() <= OUTPUT_TAIL {
        return text.to_owned();
    }
    let mut cut = text.len() - OUTPUT_TAIL;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    text[cut..].to_owned()
}

/// A tool's process group, killed if the run is abandoned before it ends.
struct Group {
    child: Child,
}

impl Drop for Group {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None))
            && let Some(pid) = self.child.id()
        {
            kill_tree(pid);
        }
    }
}

#[cfg(unix)]
fn kill_tree(pid: u32) {
    // The child leads its own group (`process_group(0)`), so this reaches
    // the engine that latexmk started as well.
    let _ = std::process::Command::new("kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(windows)]
fn kill_tree(pid: u32) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A TeX tool with the environment the log parser expects (long log lines)
/// and no window of its own.
pub(crate) fn command(tc: &Toolchain, program: &Path, args: &[String], dir: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(dir)
        .env("PATH", tc.path_env())
        .env("max_print_line", "10000")
        .env("error_line", "254")
        .env("half_error_line", "238")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
        // Windows looks for a program in the current folder (the project)
        // before `PATH`: a `pdflatex.exe` shipped with a project would run.
        cmd.env("NoDefaultCurrentDirectoryInExePath", "1");
    }
    cmd
}

async fn run(
    tc: &Toolchain,
    program: &Path,
    args: &[String],
    dir: &Path,
    deadline: Instant,
) -> Result<Run, CompileError> {
    let tool = program
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut child = command(tc, program, args, dir)
        .spawn()
        .map_err(|source| CompileError::Spawn { tool, source })?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let mut group = Group { child };
    let read = async {
        let (a, b) = tokio::join!(drain(stdout), drain(stderr));
        a + &b
    };
    let left = deadline.saturating_duration_since(Instant::now());
    let both = async {
        let (output, status) = tokio::join!(read, group.child.wait());
        (output, status)
    };
    match tokio::time::timeout(left, both).await {
        Ok((output, status)) => Ok(Run {
            success: status?.success(),
            timed_out: false,
            output: tail(&output),
        }),
        // Dropping `group` kills the tool and everything it started.
        Err(_) => Ok(Run {
            success: false,
            timed_out: true,
            output: String::new(),
        }),
    }
}

/// Read a pipe to the end, keeping the tail. Bytes, not lines: TeX output
/// is not always UTF-8, and a stalled reader would block the tool.
async fn drain(pipe: Option<impl AsyncRead + Unpin>) -> String {
    let Some(mut pipe) = pipe else {
        return String::new();
    };
    let mut kept: Vec<u8> = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match pipe.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                kept.extend_from_slice(&buf[..n]);
                if kept.len() > 2 * OUTPUT_TAIL {
                    kept.drain(..kept.len() - OUTPUT_TAIL);
                }
            }
        }
    }
    tail(&String::from_utf8_lossy(&kept))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reruns_are_requested_by_the_log() {
        assert!(wants_rerun(
            "LaTeX Warning: Label(s) may have changed. Rerun to get cross-references right."
        ));
        assert!(!wants_rerun("Output written on main.pdf (1 page)."));
    }

    #[test]
    fn tails_cut_on_char_boundaries() {
        let text = "汉".repeat(OUTPUT_TAIL);
        let t = tail(&text);
        assert!(t.len() <= OUTPUT_TAIL);
        assert!(t.starts_with('汉'));
    }

    #[test]
    fn tool_errors_come_from_the_console() {
        let issue = tool_error(
            "Running bibtex\nI couldn't open database file refs.bib\nbibtex: Error found\nLatexmk: done\n",
        );
        assert_eq!(issue.message, "bibtex: Error found");
    }

    #[test]
    fn names_a_tool_would_misread_are_refused() {
        for bad in [
            "-shell-escape",
            "-pdflatex=touch x.tex",
            "&latex",
            "\\input{x}.tex",
            "*main.tex",
            "sub/-norc.tex",
            "a\nb.tex",
            "`touch x`.tex",
            "$(touch x).tex",
            "a\"b.tex",
            "50%.tex",
            "~x.tex",
            "a;b.tex",
            "../main.tex",
            "/abs/main.tex",
            "",
        ] {
            assert!(unsafe_name(Path::new(bad)), "{bad}");
        }
        for good in [
            "main.tex",
            "sub/main.tex",
            "paper-2.tex",
            "论文.tex",
            "my paper.tex",
            "Bob's paper (v2).tex",
            ".biwrite/zh/main.tex",
            "./main.tex",
        ] {
            assert!(!unsafe_name(Path::new(good)), "{good}");
        }
    }
}

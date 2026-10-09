//! SyncTeX through the `synctex` command that TeX distributions ship:
//! source line to PDF boxes (forward) and PDF point to source line
//! (inverse). Coordinates are PDF points from the top left of the page.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::compile::command;
use crate::project::clean;
use crate::toolchain::Toolchain;

/// Where a source line is typeset.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfBox {
    /// 1-based.
    pub page: u32,
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

/// The source behind a point of the PDF.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePoint {
    /// Absolute path, `.` components removed.
    pub file: PathBuf,
    /// 1-based.
    pub line: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("the synctex tool is not installed")]
    NoTool,
    #[error("synctex did not answer: {0}")]
    Failed(String),
}

const TIMEOUT: Duration = Duration::from_secs(15);

async fn synctex(tc: &Toolchain, args: Vec<String>, dir: &Path) -> Result<String, SyncError> {
    if !tc.synctex {
        return Err(SyncError::NoTool);
    }
    let mut cmd = command(tc, &tc.tool("synctex"), &args, dir);
    cmd.kill_on_drop(true);
    let out = tokio::time::timeout(TIMEOUT, cmd.output())
        .await
        .map_err(|_| SyncError::Failed("timed out".into()))?
        .map_err(|e| SyncError::Failed(e.to_string()))?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Boxes typeset from `line` of `input` (absolute path) in `pdf`.
pub async fn forward(
    tc: &Toolchain,
    pdf: &Path,
    input: &Path,
    line: u32,
    column: u32,
) -> Result<Vec<PdfBox>, SyncError> {
    let dir = pdf.parent().unwrap_or(Path::new("."));
    let args = vec![
        "view".to_owned(),
        "-i".to_owned(),
        format!("{}:{}:{}", line.max(1), column, input.display()),
        "-o".to_owned(),
        pdf.display().to_string(),
    ];
    Ok(parse_view(&synctex(tc, args, dir).await?))
}

/// The source line under point (`x`, `y`) of `page` in `pdf`.
pub async fn inverse(
    tc: &Toolchain,
    pdf: &Path,
    page: u32,
    x: f64,
    y: f64,
) -> Result<Option<SourcePoint>, SyncError> {
    let dir = pdf.parent().unwrap_or(Path::new("."));
    let args = vec![
        "edit".to_owned(),
        "-o".to_owned(),
        format!("{}:{:.2}:{:.2}:{}", page.max(1), x, y, pdf.display()),
    ];
    Ok(parse_edit(&synctex(tc, args, dir).await?))
}

/// `key:value` lines between `SyncTeX result begin` and `end`.
fn records(out: &str) -> impl Iterator<Item = (&str, &str)> {
    out.lines()
        .skip_while(|l| !l.starts_with("SyncTeX result begin"))
        .take_while(|l| !l.starts_with("SyncTeX result end"))
        .filter_map(|l| l.split_once(':'))
}

fn parse_view(out: &str) -> Vec<PdfBox> {
    let mut boxes = Vec::new();
    let mut page = 0u32;
    let (mut h, mut v, mut w) = (0.0f64, 0.0f64, 0.0f64);
    for (key, value) in records(out) {
        let number = value.trim().parse::<f64>().unwrap_or(0.0);
        match key {
            "Page" => page = value.trim().parse().unwrap_or(0),
            "h" => h = number,
            "v" => v = number,
            "W" => w = number,
            // `v` is the baseline and `H` the height above it.
            "H" if page > 0 && w > 0.0 => {
                let height = number.max(1.0);
                let item = PdfBox {
                    page,
                    left: h,
                    top: v - height,
                    width: w,
                    height: height * 1.3,
                };
                if !boxes.contains(&item) {
                    boxes.push(item);
                }
            }
            _ => {}
        }
    }
    boxes
}

fn parse_edit(out: &str) -> Option<SourcePoint> {
    let mut file = None;
    let mut line = None;
    for (key, value) in records(out) {
        match key {
            "Input" if file.is_none() => file = Some(clean(Path::new(value.trim()))),
            "Line" if line.is_none() => line = value.trim().parse::<u32>().ok().filter(|n| *n > 0),
            _ => {}
        }
    }
    Some(SourcePoint {
        file: file?,
        line: line?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: &str = "This is SyncTeX command line utility, version 1.5
SyncTeX result begin
Output:main.pdf
Page:1
x:153.195526
y:156.585541
h:133.768356
v:158.522720
W:343.711060
H:8.855677
before:
offset:-1
middle:
after:
Output:main.pdf
Page:1
x:142.070557
y:168.540710
h:133.768356
v:170.477890
W:343.711060
H:8.855677
before:
offset:-1
middle:
after:
SyncTeX result end
";

    #[test]
    fn view_gives_one_box_per_line() {
        let boxes = parse_view(VIEW);
        assert_eq!(boxes.len(), 2);
        assert_eq!(boxes[0].page, 1);
        assert!((boxes[0].top - (158.522720 - 8.855677)).abs() < 1e-6);
        assert!((boxes[1].left - 133.768356).abs() < 1e-6);
        assert!(parse_view("SyncTeX result begin\nSyncTeX result end\n").is_empty());
    }

    #[test]
    fn edit_gives_the_file_and_line() {
        let out = "SyncTeX result begin
Output:main.pdf
Input:/tmp/paper/./sections/method.tex
Line:2
Column:-1
Offset:0
Context:
SyncTeX result end
";
        assert_eq!(
            parse_edit(out),
            Some(SourcePoint {
                file: PathBuf::from("/tmp/paper/sections/method.tex"),
                line: 2
            })
        );
        assert_eq!(
            parse_edit("SyncTeX result begin\nSyncTeX result end\n"),
            None
        );
    }
}

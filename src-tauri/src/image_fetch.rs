//! Pictures a LaTeX document names that are not on disk yet, when the
//! document also says where to get them: a web address (in a comment, a
//! link or an argument) whose last part is the picture's file name. They
//! are downloaded into the paper's folder when the author clicks, as new
//! files only, and only pictures and PDFs.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::error::{CommandError, CommandResult};
use crate::latex_commands::project;
use crate::state::Doc;

/// Extensions of the pictures looked for.
const EXTENSIONS: [&str; 8] = ["png", "jpg", "jpeg", "pdf", "eps", "svg", "gif", "webp"];
/// Largest picture downloaded.
const MAX_BYTES: usize = 25 << 20;
const TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingImage {
    /// The file, relative to the folder TeX runs in.
    pub file: String,
    /// Where the document says it comes from.
    pub url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Downloaded {
    pub file: String,
    /// Why it was not downloaded.
    pub error: Option<String>,
}

fn is_picture(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// `line` up to its first `%` that starts a comment.
fn code_of(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'%' => return &line[..i],
            _ => i += 1,
        }
    }
    line
}

/// The picture files `text` names in braces outside comments, as written.
fn named(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let code = code_of(line);
        let mut rest = code;
        while let Some(open) = rest.find('{') {
            let after = &rest[open + 1..];
            let Some(close) = after.find(['}', '{']) else {
                break;
            };
            if after.as_bytes()[close] == b'}' {
                let name = after[..close].trim();
                if is_picture(name)
                    && !name.contains("://")
                    && !name.contains(char::is_whitespace)
                    && !name.contains('#')
                    && !out.iter().any(|n| n == name)
                {
                    out.push(name.to_owned());
                }
            }
            rest = &after[close..];
        }
    }
    out
}

/// The web addresses anywhere in `text`, comments included.
fn addresses(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("http") {
        let tail = &rest[at..];
        if tail.starts_with("https://") || tail.starts_with("http://") {
            let end = tail
                .find(|c: char| {
                    c.is_whitespace() || matches!(c, '"' | '\'' | '{' | '}' | '<' | '>' | ')')
                })
                .unwrap_or(tail.len());
            out.push(&tail[..end]);
            rest = &tail[end..];
        } else {
            rest = &tail[4..];
        }
    }
    out
}

/// The last part of the path of `url`, without its query.
fn url_file(url: &str) -> &str {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.rsplit('/').next().unwrap_or("")
}

/// A relative name inside the folder: no `..`, not absolute.
fn stays_inside(name: &str) -> bool {
    let path = Path::new(name);
    !path.is_absolute()
        && path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// The folders TeX looks in for pictures: the one it runs in, then those
/// `\graphicspath` names.
fn folders(dir: &Path, root_text: &str) -> Vec<PathBuf> {
    let mut out = vec![dir.to_path_buf()];
    if let Some(at) = root_text.find("\\graphicspath{") {
        let mut rest = &root_text[at + "\\graphicspath{".len()..];
        while let Some(inner) = rest.strip_prefix('{') {
            let Some(end) = inner.find('}') else { break };
            out.push(dir.join(&inner[..end]));
            rest = &inner[end + 1..];
        }
    }
    out
}

/// The pictures `text` (the open document of a project with root text
/// `root_text`, TeX running in `dir`) names that no folder TeX looks in
/// has, each with the web address the document gives for it.
pub(crate) fn missing(dir: &Path, root_text: &str, text: &str) -> Vec<MissingImage> {
    let urls: Vec<&str> = addresses(text)
        .into_iter()
        .chain(addresses(root_text))
        .collect();
    let looked = folders(dir, root_text);
    named(text)
        .into_iter()
        .filter(|name| stays_inside(name))
        .filter(|name| !looked.iter().any(|f| f.join(name).exists()))
        .filter_map(|name| {
            let base = name.rsplit('/').next().unwrap_or(&name).to_owned();
            let url = urls.iter().find(|u| url_file(u) == base)?;
            Some(MissingImage {
                file: name.trim_start_matches("./").to_owned(),
                url: (*url).to_owned(),
            })
        })
        .collect()
}

/// Whether `bytes` begin as a picture or a PDF does.
fn looks_like_picture(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(512)];
    head.starts_with(&[0xFF, 0xD8, 0xFF])
        || head.starts_with(b"\x89PNG")
        || head.starts_with(b"%PDF")
        || head.starts_with(b"GIF8")
        || head.starts_with(b"%!PS")
        || (head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP"))
        || String::from_utf8_lossy(head).contains("<svg")
}

async fn fetch(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    if response
        .content_length()
        .is_some_and(|n| n as usize > MAX_BYTES)
    {
        return Err("larger than 25 MB".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        bytes.extend_from_slice(&chunk);
        if bytes.len() > MAX_BYTES {
            return Err("larger than 25 MB".into());
        }
    }
    if !looks_like_picture(&bytes) {
        return Err("not a picture".into());
    }
    Ok(bytes)
}

/// `bytes` as the new file `path`, never over one that is there.
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| {
        let _ = std::fs::remove_file(path);
        e.to_string()
    })
}

/// The pictures the open document `text` names but nobody downloaded yet,
/// with their web addresses (see the module).
#[tauri::command]
pub async fn latex_missing_images(state: Doc, text: String) -> CommandResult<Vec<MissingImage>> {
    let Ok(p) = project(&state) else {
        return Ok(Vec::new());
    };
    Ok(missing(&p.dir, &p.root_text, &text))
}

/// Download the pictures [`latex_missing_images`] finds into the folder
/// TeX runs in.
#[tauri::command]
pub async fn latex_download_images(state: Doc, text: String) -> CommandResult<Vec<Downloaded>> {
    let p = project(&state)?;
    let wanted = missing(&p.dir, &p.root_text, &text);
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(concat!("BiWrite/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| CommandError::Settings(e.to_string()))?;
    let mut out = Vec::new();
    for image in wanted {
        let path = p.dir.join(&image.file);
        let result = match fetch(&client, &image.url).await {
            Ok(bytes) => write_new(&path, &bytes),
            Err(e) => Err(e),
        };
        match &result {
            Ok(()) => log::info!("downloaded {} for the paper", image.file),
            Err(e) => log::warn!("could not download {}: {e}", image.file),
        }
        out.push(Downloaded {
            file: image.file,
            error: result.err(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "% curl -fL \"https://raw.example.org/docs/images/zh/editor.jpg\" -o editor.jpg\n\
% curl -fL \"https://raw.example.org/docs/images/zh/assistant.jpg\" -o assistant.jpg\n\
\\documentclass{ctexart}\n\
\\begin{document}\n\
\\projectimage\n  {editor.jpg}\n  {https://raw.example.org/docs/images/zh/editor.jpg}\n  {left}\n\
\\projectimage{assistant.jpg}{https://raw.example.org/docs/images/zh/assistant.jpg}{right}\n\
\\includegraphics{figures/here.png}\n\
\\includegraphics{figures/nowhere.png}\n\
% \\includegraphics{commented.png} https://raw.example.org/commented.png\n\
\\end{document}\n";

    #[test]
    fn pictures_not_on_disk_are_found_with_their_addresses() {
        let dir = std::env::temp_dir().join(format!("biwrite-images-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("figures")).unwrap();
        std::fs::write(dir.join("figures/here.png"), b"\x89PNG").unwrap();
        let found = missing(&dir, DOC, DOC);
        assert_eq!(
            found,
            [
                MissingImage {
                    file: "editor.jpg".into(),
                    url: "https://raw.example.org/docs/images/zh/editor.jpg".into()
                },
                MissingImage {
                    file: "assistant.jpg".into(),
                    url: "https://raw.example.org/docs/images/zh/assistant.jpg".into()
                },
            ],
            "figures/here.png is there, nowhere.png has no address, a comment names nothing"
        );
        std::fs::write(dir.join("editor.jpg"), b"\xFF\xD8\xFF").unwrap();
        assert_eq!(missing(&dir, DOC, DOC).len(), 1, "downloaded once");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A real download: the README's picture of the editor.
    #[tokio::test]
    #[ignore = "downloads from the network"]
    async fn a_picture_comes_from_its_address() {
        let client = reqwest::Client::builder().timeout(TIMEOUT).build().unwrap();
        let bytes = fetch(
            &client,
            "https://raw.githubusercontent.com/zzccppp/biwrite/master/docs/images/zh/editor.jpg",
        )
        .await
        .unwrap();
        assert!(bytes.starts_with(&[0xFF, 0xD8, 0xFF]), "a JPEG");
        assert!(
            fetch(
                &client,
                "https://raw.githubusercontent.com/zzccppp/biwrite/master/README.md"
            )
            .await
            .is_err(),
            "not a picture"
        );
    }

    #[test]
    fn only_new_pictures_inside_the_folder() {
        assert!(stays_inside("figures/a.png"));
        assert!(!stays_inside("../a.png"));
        assert!(!stays_inside("/etc/a.png"));
        assert!(looks_like_picture(b"\xFF\xD8\xFF\xE0"));
        assert!(looks_like_picture(b"%PDF-1.7"));
        assert!(!looks_like_picture(b"<html>not found</html>"));
        let dir = std::env::temp_dir().join(format!("biwrite-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.png");
        std::fs::write(&path, b"mine").unwrap();
        assert!(write_new(&path, b"theirs").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"mine", "never over a file");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

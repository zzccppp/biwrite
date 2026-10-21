//! Write through the system clipboard utility, outside the webview.

use std::io::Write;
use std::process::{Command, Stdio};

/// The key stays in Rust and is passed on stdin, never as a process argument.
pub fn copy(text: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("/usr/bin/pbcopy");
        command.env("LC_ALL", "en_US.UTF-8");
        command
    };
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("clip.exe");
        command.creation_flags(0x0800_0000);
        command
    };
    #[cfg(not(any(target_os = "macos", windows)))]
    let mut command = {
        let mut command = Command::new("xclip");
        command.args(["-selection", "clipboard"]);
        command
    };

    // clip.exe otherwise interprets bytes in the current Windows code page.
    #[cfg(windows)]
    let encoded = windows_input(text);
    #[cfg(windows)]
    let input = encoded.as_slice();
    #[cfg(not(windows))]
    let input = text.as_bytes();
    write_to_command(&mut command, input)
}

#[cfg(any(windows, test))]
fn windows_input(text: &str) -> Vec<u8> {
    std::iter::once(0xfeff_u16)
        .chain(text.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn write_to_command(command: &mut Command, input: &[u8]) -> Result<(), String> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("the clipboard could not be reached: {e}"))?;
    let written = match child.stdin.take() {
        Some(mut stdin) => stdin.write_all(input),
        None => Err(std::io::Error::other("the clipboard input is unavailable")),
    };
    // The pipe has been closed above. Reap failed writers too, so a helper
    // cannot remain waiting for input after the command has returned an error.
    if let Err(e) = written {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("the clipboard could not be written: {e}"));
    }
    let status = match child.wait() {
        Ok(status) => status,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("the clipboard could not be written: {e}"));
        }
    };
    if status.success() {
        Ok(())
    } else {
        Err(format!("the clipboard could not be written ({status})"))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn windows_receives_unicode_with_a_byte_order_mark() {
        assert_eq!(
            windows_input("sk-中🔑+/="),
            [
                0xff, 0xfe, b's', 0, b'k', 0, b'-', 0, 0x2d, 0x4e, 0x3d, 0xd8, 0x11, 0xdd, b'+', 0,
                b'/', 0, b'=', 0,
            ]
        );
    }

    /// Changes the system clipboard. Run only in a disposable CI runner,
    /// never as part of the user's normal local test run.
    #[cfg(any(target_os = "macos", windows))]
    #[test]
    #[ignore = "changes the system clipboard; run only in a disposable environment"]
    fn native_clipboard_round_trip() {
        let input = "sk-biwrite-synthetic-0123456789+/=_中文🔑";
        copy(input).unwrap();
        #[cfg(target_os = "macos")]
        let mut reader = {
            let mut command = Command::new("/usr/bin/pbpaste");
            command.env("LC_ALL", "en_US.UTF-8");
            command
        };
        #[cfg(windows)]
        let mut reader = {
            use std::os::windows::process::CommandExt;
            let mut command = Command::new("powershell.exe");
            command.args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); [Console]::Write((Get-Clipboard -Raw))",
            ]);
            command.creation_flags(0x0800_0000);
            command
        };
        let output = reader.output().unwrap();
        assert!(
            output.status.success(),
            "clipboard reader failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(text.trim_end_matches(['\r', '\n']), input);
    }

    #[cfg(unix)]
    #[test]
    fn the_pipe_preserves_the_text_without_adding_a_newline() {
        let path = std::env::temp_dir().join(format!(
            "biwrite-clipboard-input-{}.txt",
            std::process::id()
        ));
        let input = "sk-synthetic-0123456789+/=中🔑";
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "cat > \"$1\"", "clipboard-test"]);
        command.arg(&path);
        let result = write_to_command(&mut command, input.as_bytes());
        let written = std::fs::read(&path);
        let _ = std::fs::remove_file(&path);
        result.unwrap();
        assert_eq!(written.unwrap(), input.as_bytes());
    }

    #[cfg(unix)]
    #[test]
    fn an_unsuccessful_helper_is_an_error() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "cat >/dev/null; exit 7"]);
        let error = write_to_command(&mut command, b"sk-synthetic").unwrap_err();
        assert!(error.contains("clipboard could not be written"), "{error}");
        assert!(error.contains('7'), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_closed_input_pipe_is_an_error() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exec 0<&-; exec sleep 10"]);
        let error = write_to_command(&mut command, &vec![b'x'; 1024 * 1024]).unwrap_err();
        assert!(error.contains("clipboard could not be written"), "{error}");
    }
}

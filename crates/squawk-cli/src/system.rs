//! The two things the CLI and TUI ask of macOS: put text on the clipboard,
//! and open a file for editing.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// `pbcopy`: no clipboard crate, and it behaves exactly like the user's own
/// `| pbcopy` would.
pub fn copy(text: &str) -> anyhow::Result<()> {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(text.as_bytes())?;
    let status = child.wait()?;
    anyhow::ensure!(status.success(), "pbcopy failed");
    Ok(())
}

/// The editor command line: `$VISUAL`, else `$EDITOR`, else `open` (the
/// file's default app). Split on whitespace so `code -w` works.
pub fn editor_argv(env: impl Fn(&str) -> Option<String>) -> Vec<String> {
    ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|k| env(k))
        .map(|v| v.split_whitespace().map(str::to_string).collect::<Vec<_>>())
        .find(|argv| !argv.is_empty())
        .unwrap_or_else(|| vec!["open".to_string()])
}

/// Run the editor on `path` in this terminal and wait for it.
pub fn edit(path: &Path) -> anyhow::Result<()> {
    let argv = editor_argv(|k| std::env::var(k).ok());
    let status = Command::new(&argv[0]).args(&argv[1..]).arg(path).status()?;
    anyhow::ensure!(status.success(), "{} exited with {status}", argv[0]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_prefers_visual_then_editor_then_open() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(name, _)| *name == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(
            editor_argv(env(&[("VISUAL", "code -w"), ("EDITOR", "vim")])),
            ["code", "-w"]
        );
        assert_eq!(editor_argv(env(&[("EDITOR", "hx")])), ["hx"]);
        assert_eq!(
            editor_argv(env(&[("VISUAL", "  "), ("EDITOR", "vim")])),
            ["vim"]
        );
        assert_eq!(editor_argv(env(&[])), ["open"]);
    }
}

//! The agent: a long-running Claude Code session inside a tmux session,
//! dictated to with ⌥+fn from anywhere.
//!
//! Text reaches it as keystrokes, `tmux send-keys -t =<session>: -l --
//! <text>` then `tmux send-keys -t =<session>: Enter` (`=`: that session
//! exactly, never one whose name merely starts the same): never the clipboard, never
//! cmd+V, and no window is activated, so whatever has focus keeps it.
//!
//! When the session is not there, squawk starts it (`tmux new-session -d`
//! in the agent's directory), types the command into its shell, and opens
//! a terminal window attached to it. The command is typed rather than given
//! to `new-session`: a tmux server that squawk starts has the app's bare
//! launchd `PATH`, while the session's own login shell reads the user's
//! profile and finds `claude` wherever it is installed.
//!
//! Everything here is the app's and the CLI's alike: what to send
//! ([`messages`]), the tmux and terminal command lines ([`Setup`]), and the
//! sequence that runs them ([`deliver`], [`show`]) through a [`Shell`], so
//! the sequence is tested with a fake one. No AppKit; hiding the terminal
//! is the app's.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::AgentConfig;

/// What a clear phrase, or a long idle, sends.
pub const CLEAR: &str = "/clear";
/// The pause between `/clear` and the request after it, so Claude Code has
/// cleared before the request arrives.
pub const BETWEEN_MESSAGES: Duration = Duration::from_millis(300);
/// After a launch, how long Claude Code gets once it is running before the
/// first text is typed.
pub const BOOT_WAIT: Duration = Duration::from_secs(2);
/// After a launch, the longest wait for the shell to hand over to the
/// command; past it the text is sent anyway.
pub const BOOT_TIMEOUT: Duration = Duration::from_secs(15);
const BOOT_POLL: Duration = Duration::from_millis(250);

/// Where tmux is looked for after `PATH` (an app launched from Finder has
/// only `/usr/bin:/bin:/usr/sbin:/sbin`).
pub const TMUX_CANDIDATES: &[&str] = &[
    "/opt/homebrew/bin/tmux",
    "/usr/local/bin/tmux",
    "/opt/local/bin/tmux",
    "/usr/bin/tmux",
];

/// Shells a fresh pane runs before the command takes over.
const SHELLS: &[&str] = &[
    "zsh", "bash", "sh", "fish", "dash", "ksh", "tcsh", "csh", "nu", "login",
];

/// Whether `text` is exactly one of `phrases`: case-insensitive, surrounding
/// space and end punctuation ignored ("New conversation." matches "new
/// conversation"; "start a new conversation about X" does not).
pub fn is_clear_phrase(text: &str, phrases: &[String]) -> bool {
    let said = phrase_key(text);
    !said.is_empty() && phrases.iter().any(|p| phrase_key(p) == said)
}

/// Lowercased, end punctuation off, inner whitespace collapsed.
fn phrase_key(text: &str) -> String {
    let trimmed = text
        .trim()
        .trim_end_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace() || c == '…');
    trimmed
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Whether the agent has been left alone long enough that the next request
/// starts a new conversation. `mins` 0 never expires, and neither does a
/// run with no request yet (a session squawk has not talked to may be one
/// the user is in the middle of).
pub fn idle_expired(last_request: Option<Instant>, now: Instant, mins: u64) -> bool {
    match last_request {
        Some(at) if mins > 0 => now.saturating_duration_since(at) >= Duration::from_secs(mins * 60),
        _ => false,
    }
}

/// What to type into the session for one dictation, in order, each followed
/// by Enter: `/clear` alone for a clear phrase; `/clear` then the text when
/// the agent sat idle too long; else the text. Line breaks become spaces,
/// since each one would submit.
pub fn messages(text: &str, clear_phrases: &[String], idle_expired: bool) -> Vec<String> {
    if is_clear_phrase(text, clear_phrases) {
        return vec![CLEAR.to_string()];
    }
    let text = one_line(text);
    if text.is_empty() {
        return Vec::new();
    }
    if idle_expired {
        vec![CLEAR.to_string(), text]
    } else {
        vec![text]
    }
}

fn one_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The first tmux on `PATH`, else the first of [`TMUX_CANDIDATES`] that
/// exists, else plain `tmux`.
pub fn find_tmux(path_var: Option<&OsStr>, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let on_path = path_var
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|dir| dir.join("tmux"));
    on_path
        .chain(TMUX_CANDIDATES.iter().map(PathBuf::from))
        .find(|p| exists(p))
        .unwrap_or_else(|| PathBuf::from("tmux"))
}

/// One command line to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub program: PathBuf,
    pub args: Vec<String>,
}

impl Cmd {
    fn new(program: impl Into<PathBuf>, args: &[&str]) -> Cmd {
        Cmd {
            program: program.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
        }
    }

    /// The program's file name and the first argument ("tmux has-session"),
    /// for error messages. Never the rest: it can be dictated text.
    pub fn label(&self) -> String {
        let name = self
            .program
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match self.args.first() {
            Some(first) if !first.starts_with('-') => format!("{name} {first}"),
            _ => name,
        }
    }
}

/// The agent's settings, resolved: tmux found, `~` expanded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setup {
    pub tmux: PathBuf,
    pub session: String,
    pub dir: PathBuf,
    pub command: String,
    pub terminal: String,
}

impl Setup {
    /// From the config, with tmux looked up on this machine.
    pub fn new(config: &AgentConfig, home: &Path) -> Setup {
        let path = std::env::var_os("PATH");
        Setup::with_tmux(config, home, find_tmux(path.as_deref(), |p| p.is_file()))
    }

    pub fn with_tmux(config: &AgentConfig, home: &Path, tmux: PathBuf) -> Setup {
        Setup {
            tmux,
            session: config.tmux_session.trim().to_string(),
            dir: config.dir_path(home),
            command: config.command.trim().to_string(),
            terminal: config.terminal.trim().to_string(),
        }
    }

    fn tmux(&self, args: &[&str]) -> Cmd {
        Cmd::new(&self.tmux, args)
    }

    /// The session as a tmux target, exact: a bare `agent` would also
    /// match a session called `agent-old` when `agent` is not there.
    fn session_target(&self) -> String {
        format!("={}", self.session)
    }

    /// The session's current pane as a tmux target.
    fn pane_target(&self) -> String {
        format!("={}:", self.session)
    }

    /// `tmux has-session -t =<session>`: succeeds when it is there.
    pub fn has_session(&self) -> Cmd {
        self.tmux(&["has-session", "-t", &self.session_target()])
    }

    /// `tmux new-session -d -s <session> -c <dir>`.
    pub fn new_session(&self) -> Cmd {
        let dir = self.dir.to_string_lossy();
        self.tmux(&["new-session", "-d", "-s", &self.session, "-c", &dir])
    }

    /// Type `text` as is (`-l`: no key names), without submitting it.
    pub fn send_text(&self, text: &str) -> Cmd {
        self.tmux(&["send-keys", "-t", &self.pane_target(), "-l", "--", text])
    }

    /// Press Enter in the session.
    pub fn send_enter(&self) -> Cmd {
        self.tmux(&["send-keys", "-t", &self.pane_target(), "Enter"])
    }

    /// What the session's pane is running now (`zsh`, then `claude`).
    pub fn pane_command(&self) -> Cmd {
        self.tmux(&[
            "display-message",
            "-p",
            "-t",
            &self.pane_target(),
            "#{pane_current_command}",
        ])
    }

    /// The terminals attached to the session, one tty per line.
    pub fn list_clients(&self) -> Cmd {
        self.tmux(&[
            "list-clients",
            "-t",
            &self.session_target(),
            "-F",
            "#{client_tty}",
        ])
    }

    /// `tmux attach -t <session>` as one shell line, the tmux path in full
    /// (a terminal's command may not get the user's `PATH`).
    pub fn attach_line(&self) -> String {
        format!(
            "{} attach -t {}",
            shell_quote(&self.tmux.to_string_lossy()),
            shell_quote(&self.session_target())
        )
    }

    /// Open the terminal app on the session: a new tab in Ghostty's front
    /// window (a new window if there is none), a new window elsewhere. A
    /// terminal squawk does not know how to hand a command to is just
    /// brought forward.
    pub fn open_window(&self) -> Cmd {
        let tmux = self.tmux.to_string_lossy();
        let target = self.session_target();
        let attach: [&str; 4] = [&tmux, "attach", "-t", &target];
        let open_new = |app: &str, pre: &[&str]| {
            let mut args = vec!["-na", app, "--args"];
            args.extend_from_slice(pre);
            args.extend_from_slice(&attach);
            Cmd::new("/usr/bin/open", &args)
        };
        match terminal_key(&self.terminal).as_str() {
            // Ghostty 1.3 has an AppleScript dictionary: a new tab in the
            // front window if there is one, else a new window. Inside the
            // tell block `tab` is a class, hence `new tab in front window`.
            "ghostty" => osascript(&[
                "tell application id \"com.mitchellh.ghostty\"",
                "set cfg to new surface configuration",
                &format!(
                    "set command of cfg to {}",
                    applescript_string(&self.attach_line())
                ),
                "if (count of windows) > 0 then",
                "new tab in front window with configuration cfg",
                "else",
                "new window with configuration cfg",
                "end if",
                "activate",
                "end tell",
            ]),
            "alacritty" => open_new("Alacritty", &["-e"]),
            "kitty" => open_new("kitty", &[]),
            "wezterm" => open_new("WezTerm", &["start", "--"]),
            "terminal" => osascript(&[
                "tell application id \"com.apple.Terminal\"",
                "activate",
                &format!("do script {}", applescript_string(&self.attach_line())),
                "end tell",
            ]),
            "iterm" | "iterm2" => osascript(&[
                "tell application id \"com.googlecode.iterm2\"",
                "activate",
                &format!(
                    "create window with default profile command {}",
                    applescript_string(&self.attach_line())
                ),
                "end tell",
            ]),
            _ => self.activate(),
        }
    }

    /// Bring the terminal app forward (`open -a <terminal>`), without
    /// choosing a window.
    pub fn activate(&self) -> Cmd {
        Cmd::new("/usr/bin/open", &["-a", &self.terminal])
    }

    /// Whether an app called `name` (the front app's localized name) is
    /// this setup's terminal.
    pub fn is_terminal(&self, name: &str) -> bool {
        let (a, b) = (terminal_key(name), terminal_key(&self.terminal));
        !a.is_empty() && (a == b || (a.starts_with("iterm") && b.starts_with("iterm")))
    }
}

/// "iTerm2.app" → "iterm2", "Ghostty" → "ghostty".
fn terminal_key(name: &str) -> String {
    let name = name.trim();
    let name = name.strip_suffix(".app").unwrap_or(name);
    name.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase()
}

fn osascript(lines: &[&str]) -> Cmd {
    let mut args = Vec::new();
    for line in lines {
        args.push("-e");
        args.push(line);
    }
    Cmd::new("/usr/bin/osascript", &args)
}

/// Single-quoted for sh, unless it is plainly safe as is. `=` is never
/// safe: zsh expands a word starting with it (`=agent`) to a command path.
pub fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+:@%,".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// How a command went.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ran {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Runs commands and waits. [`System`] for real; tests record instead.
pub trait Shell {
    fn run(&mut self, cmd: &Cmd) -> io::Result<Ran>;
    fn sleep(&mut self, duration: Duration);
}

/// The real thing: each command a child process, stdin closed, output
/// captured.
pub struct System;

impl Shell for System {
    fn run(&mut self, cmd: &Cmd) -> io::Result<Ran> {
        let out = Command::new(&cmd.program)
            .args(&cmd.args)
            // Inside a pane (`squawk agent send` typed in tmux), $TMUX
            // points tmux at that pane's server; without it every call
            // reaches the default server, as it does from the app.
            .env_remove("TMUX")
            .stdin(Stdio::null())
            .output()?;
        Ok(Ran {
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        })
    }

    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Run `cmd`; a failure is an error with tmux's own words.
fn must(shell: &mut dyn Shell, cmd: &Cmd) -> Result<Ran, String> {
    match shell.run(cmd) {
        Ok(ran) if ran.success => Ok(ran),
        Ok(ran) if ran.stderr.is_empty() => Err(format!("{} failed", cmd.label())),
        Ok(ran) => Err(format!("{} failed: {}", cmd.label(), ran.stderr)),
        Err(e) => Err(format!("could not run {}: {e}", cmd.label())),
    }
}

/// Make sure the session is there. Returns whether it had to be launched
/// (the session started, the command typed into it, a terminal window
/// opened on it). A window that would not open is logged, not an error: the
/// session works without one.
pub fn ensure_session(shell: &mut dyn Shell, setup: &Setup) -> Result<bool, String> {
    if shell.run(&setup.has_session()).is_ok_and(|r| r.success) {
        return Ok(false);
    }
    if !setup.dir.exists() {
        if let Err(e) = std::fs::create_dir_all(&setup.dir) {
            log::warn!("agent: could not create {}: {e}", setup.dir.display());
        }
    }
    must(shell, &setup.new_session())?;
    if !setup.command.is_empty() {
        must(shell, &setup.send_text(&setup.command))?;
        must(shell, &setup.send_enter())?;
    }
    if let Err(e) = must(shell, &setup.open_window()) {
        log::warn!("agent: could not open a {} window: {e}", setup.terminal);
    }
    log::info!(target: "agent", "launched tmux session {}", setup.session);
    Ok(true)
}

/// After a launch: wait until the pane runs something other than its shell
/// (the command has started), at most [`BOOT_TIMEOUT`], then [`BOOT_WAIT`]
/// more for it to be ready to read.
pub fn wait_for_boot(shell: &mut dyn Shell, setup: &Setup) {
    let polls = (BOOT_TIMEOUT.as_millis() / BOOT_POLL.as_millis()) as usize;
    for _ in 0..polls {
        if running(shell, setup).is_some_and(|r| !is_shell(&r)) {
            break;
        }
        shell.sleep(BOOT_POLL);
    }
    shell.sleep(BOOT_WAIT);
}

fn is_shell(command: &str) -> bool {
    let name = command.trim_start_matches('-');
    let name = name.rsplit('/').next().unwrap_or(name);
    SHELLS.contains(&name)
}

/// How a delivery went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Delivered {
    /// The session was not there and was launched first.
    pub launched: bool,
}

/// What the session's pane runs now, when tmux says.
fn running(shell: &mut dyn Shell, setup: &Setup) -> Option<String> {
    shell
        .run(&setup.pane_command())
        .ok()
        .filter(|r| r.success)
        .map(|r| r.stdout.trim().to_string())
        .filter(|r| !r.is_empty())
}

/// Type each of `messages` into the session, each followed by Enter, with
/// [`BETWEEN_MESSAGES`] between them; launch the session first if needed.
/// A session whose command has exited (its pane back at the shell prompt)
/// gets the command again first: the text must never run as a shell line.
pub fn deliver(
    shell: &mut dyn Shell,
    setup: &Setup,
    messages: &[String],
) -> Result<Delivered, String> {
    let launched = ensure_session(shell, setup)?;
    if launched {
        wait_for_boot(shell, setup);
    } else if running(shell, setup).is_some_and(|r| is_shell(&r)) {
        if setup.command.is_empty() {
            return Err(format!(
                "tmux session {} is at a shell prompt, not the agent",
                setup.session
            ));
        }
        must(shell, &setup.send_text(&setup.command))?;
        must(shell, &setup.send_enter())?;
        log::info!(target: "agent", "restarted the command in {}", setup.session);
        wait_for_boot(shell, setup);
    }
    for (i, message) in messages.iter().enumerate() {
        if i > 0 {
            shell.sleep(BETWEEN_MESSAGES);
        }
        must(shell, &setup.send_text(message))?;
        must(shell, &setup.send_enter())?;
    }
    Ok(Delivered { launched })
}

/// Whether a terminal is attached to the session.
pub fn attached(shell: &mut dyn Shell, setup: &Setup) -> bool {
    shell
        .run(&setup.list_clients())
        .is_ok_and(|r| r.success && !r.stdout.trim().is_empty())
}

/// Show the session: launch it (which opens its window) if it is not
/// there; else bring the terminal forward, opening a window on the session
/// first if none is attached.
pub fn show(shell: &mut dyn Shell, setup: &Setup) -> Result<(), String> {
    if ensure_session(shell, setup)? {
        return Ok(());
    }
    if attached(shell, setup) {
        must(shell, &setup.activate())?;
    } else {
        must(shell, &setup.open_window())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phrases() -> Vec<String> {
        AgentConfig::default().clear_phrases
    }

    fn setup() -> Setup {
        Setup::with_tmux(
            &AgentConfig::default(),
            Path::new("/Users/you"),
            PathBuf::from("/opt/homebrew/bin/tmux"),
        )
    }

    /// Records every command; answers from a script keyed on the tmux
    /// subcommand (or the program, for open/osascript).
    struct Fake {
        log: Vec<String>,
        answers: Vec<(&'static str, Ran)>,
        slept: Duration,
    }

    impl Fake {
        fn new() -> Fake {
            Fake {
                log: Vec::new(),
                answers: Vec::new(),
                slept: Duration::ZERO,
            }
        }

        fn answer(mut self, key: &'static str, success: bool, stdout: &str) -> Fake {
            self.answers.push((
                key,
                Ran {
                    success,
                    stdout: stdout.into(),
                    stderr: String::new(),
                },
            ));
            self
        }
    }

    impl Shell for Fake {
        fn run(&mut self, cmd: &Cmd) -> io::Result<Ran> {
            self.log.push(cmd.args.join(" "));
            let key = cmd.label();
            let ok = Ran {
                success: true,
                ..Ran::default()
            };
            // The first matching answer is used up, so a script can say
            // "zsh, then claude".
            match self.answers.iter().position(|(k, _)| key.ends_with(k)) {
                Some(i) => Ok(self.answers.remove(i).1),
                None => Ok(ok),
            }
        }

        fn sleep(&mut self, duration: Duration) {
            self.slept += duration;
        }
    }

    #[test]
    fn clear_phrases_match_whole_and_loosely() {
        let p = phrases();
        assert!(is_clear_phrase("new conversation", &p));
        assert!(is_clear_phrase("New conversation.", &p));
        assert!(is_clear_phrase("  Clear   conversation!  ", &p));
        assert!(is_clear_phrase("New conversation…", &p));
        assert!(!is_clear_phrase("Start a new conversation about tmux.", &p));
        assert!(!is_clear_phrase("new", &p));
        assert!(!is_clear_phrase("", &p));
        assert!(!is_clear_phrase(".", &[".".to_string()]));
    }

    #[test]
    fn messages_for_a_clear_an_idle_and_a_plain_request() {
        let p = phrases();
        assert_eq!(messages("New conversation.", &p, false), ["/clear"]);
        // A clear phrase after a long idle is still one /clear.
        assert_eq!(messages("New conversation.", &p, true), ["/clear"]);
        assert_eq!(
            messages("Open my notes.", &p, true),
            ["/clear", "Open my notes."]
        );
        assert_eq!(messages("Open my notes.", &p, false), ["Open my notes."]);
        assert_eq!(
            messages("First line.\n\nSecond line.", &p, false),
            ["First line. Second line."]
        );
        assert!(messages("  ", &p, true).is_empty());
    }

    #[test]
    fn idle_expires_after_the_configured_minutes() {
        let t0 = Instant::now();
        let hour = Duration::from_secs(3600);
        assert!(!idle_expired(None, t0 + hour, 60));
        assert!(!idle_expired(
            Some(t0),
            t0 + hour - Duration::from_secs(1),
            60
        ));
        assert!(idle_expired(Some(t0), t0 + hour, 60));
        assert!(!idle_expired(Some(t0), t0 + 10 * hour, 0));
    }

    #[test]
    fn tmux_is_found_on_path_then_in_the_usual_places() {
        let path = std::env::join_paths(["/usr/bin", "/custom/bin"]).unwrap();
        let only = |want: &'static str| move |p: &Path| p == Path::new(want);
        assert_eq!(
            find_tmux(Some(&path), only("/custom/bin/tmux")),
            Path::new("/custom/bin/tmux")
        );
        // The app's launchd PATH has no Homebrew.
        assert_eq!(
            find_tmux(Some(&path), only("/opt/homebrew/bin/tmux")),
            Path::new("/opt/homebrew/bin/tmux")
        );
        assert_eq!(find_tmux(None, |_| false), Path::new("tmux"));
    }

    #[test]
    fn tmux_command_lines() {
        let s = setup();
        assert_eq!(s.has_session().args, ["has-session", "-t", "=agent"]);
        assert_eq!(
            s.new_session().args,
            ["new-session", "-d", "-s", "agent", "-c", "/Users/you/agent"]
        );
        // Literal, and after `--` so text starting with "-" is not a flag.
        assert_eq!(
            s.send_text("-v is verbose; C-c").args,
            [
                "send-keys",
                "-t",
                "=agent:",
                "-l",
                "--",
                "-v is verbose; C-c"
            ]
        );
        assert_eq!(s.send_enter().args, ["send-keys", "-t", "=agent:", "Enter"]);
        assert_eq!(s.send_enter().program, Path::new("/opt/homebrew/bin/tmux"));
        // Labels never carry the text.
        assert_eq!(s.send_text("secret").label(), "tmux send-keys");
    }

    #[test]
    fn terminal_windows_attach_to_the_session() {
        let mut s = setup();
        let ghostty = s.open_window();
        assert_eq!(ghostty.program, Path::new("/usr/bin/osascript"));
        assert!(ghostty.args.contains(
            &"set command of cfg to \"/opt/homebrew/bin/tmux attach -t '=agent'\"".to_string()
        ));
        assert!(ghostty
            .args
            .contains(&"new tab in front window with configuration cfg".to_string()));

        s.terminal = "Terminal".into();
        let terminal = s.open_window();
        assert_eq!(terminal.program, Path::new("/usr/bin/osascript"));
        assert!(terminal
            .args
            .contains(&"do script \"/opt/homebrew/bin/tmux attach -t '=agent'\"".to_string()));

        s.terminal = "iTerm2".into();
        s.session = "my agent".into();
        let iterm = s.open_window();
        assert!(
            iterm.args.contains(
                &"create window with default profile command \"/opt/homebrew/bin/tmux attach -t '=my agent'\""
                    .to_string()
            ),
            "{:?}",
            iterm.args
        );

        s.terminal = "Hyper".into();
        assert_eq!(s.open_window().args, ["-a", "Hyper"]);
    }

    #[test]
    fn the_front_app_is_matched_to_the_terminal_loosely() {
        let mut s = setup();
        assert!(s.is_terminal("Ghostty"));
        assert!(s.is_terminal("ghostty"));
        assert!(!s.is_terminal("Terminal"));
        s.terminal = "iTerm2".into();
        assert!(s.is_terminal("iTerm2"));
        assert!(s.is_terminal("iTerm"));
        assert!(!s.is_terminal(""));
    }

    #[test]
    fn quoting() {
        assert_eq!(
            shell_quote("/opt/homebrew/bin/tmux"),
            "/opt/homebrew/bin/tmux"
        );
        assert_eq!(shell_quote("my agent"), "'my agent'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("=agent"), "'=agent'");
        assert_eq!(applescript_string(r#"a "b" \c"#), r#""a \"b\" \\c""#);
    }

    #[test]
    fn delivering_to_a_running_session_types_and_presses_enter() {
        let mut sh = Fake::new();
        let got = deliver(
            &mut sh,
            &setup(),
            &["/clear".into(), "Open my notes.".into()],
        )
        .unwrap();
        assert!(!got.launched);
        assert_eq!(
            sh.log,
            [
                "has-session -t =agent",
                "display-message -p -t =agent: #{pane_current_command}",
                "send-keys -t =agent: -l -- /clear",
                "send-keys -t =agent: Enter",
                "send-keys -t =agent: -l -- Open my notes.",
                "send-keys -t =agent: Enter",
            ]
        );
        assert_eq!(sh.slept, BETWEEN_MESSAGES);
    }

    #[test]
    fn delivering_with_no_session_launches_it_and_waits_for_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = setup();
        s.dir = dir.path().join("agent");
        let mut sh = Fake::new()
            .answer("tmux has-session", false, "")
            .answer("tmux display-message", true, "zsh\n")
            .answer("tmux display-message", true, "claude\n");
        let got = deliver(&mut sh, &s, &["Hello.".into()]).unwrap();
        assert!(got.launched);
        assert!(s.dir.is_dir(), "the agent's directory is created");
        let new_session = format!("new-session -d -s agent -c {}", s.dir.display());
        assert_eq!(
            sh.log,
            [
                "has-session -t =agent",
                new_session.as_str(),
                "send-keys -t =agent: -l -- claude",
                "send-keys -t =agent: Enter",
                "-e tell application id \"com.mitchellh.ghostty\" -e set cfg to new surface configuration -e set command of cfg to \"/opt/homebrew/bin/tmux attach -t '=agent'\" -e if (count of windows) > 0 then -e new tab in front window with configuration cfg -e else -e new window with configuration cfg -e end if -e activate -e end tell",
                "display-message -p -t =agent: #{pane_current_command}",
                "display-message -p -t =agent: #{pane_current_command}",
                "send-keys -t =agent: -l -- Hello.",
                "send-keys -t =agent: Enter",
            ]
        );
        // One poll's wait while the shell was still in front, then the boot wait.
        assert_eq!(sh.slept, BOOT_POLL + BOOT_WAIT);
    }

    #[test]
    fn a_session_back_at_its_shell_gets_the_command_before_the_text() {
        let mut sh = Fake::new()
            .answer("tmux display-message", true, "zsh\n")
            .answer("tmux display-message", true, "zsh\n")
            .answer("tmux display-message", true, "claude\n");
        let got = deliver(&mut sh, &setup(), &["rm -rf build".into()]).unwrap();
        assert!(!got.launched);
        assert_eq!(
            sh.log,
            [
                "has-session -t =agent",
                "display-message -p -t =agent: #{pane_current_command}",
                "send-keys -t =agent: -l -- claude",
                "send-keys -t =agent: Enter",
                "display-message -p -t =agent: #{pane_current_command}",
                "display-message -p -t =agent: #{pane_current_command}",
                "send-keys -t =agent: -l -- rm -rf build",
                "send-keys -t =agent: Enter",
            ]
        );
    }

    #[test]
    fn a_failed_send_is_an_error_naming_tmux() {
        let mut sh = Fake::new().answer("tmux send-keys", false, "");
        let err = deliver(&mut sh, &setup(), &["x".into()]).unwrap_err();
        assert_eq!(err, "tmux send-keys failed");
    }

    #[test]
    fn showing_brings_the_attached_terminal_forward_or_opens_a_window() {
        let mut sh = Fake::new().answer("tmux list-clients", true, "/dev/ttys004\n");
        show(&mut sh, &setup()).unwrap();
        assert_eq!(sh.log.last().unwrap(), "-a Ghostty");

        let mut sh = Fake::new().answer("tmux list-clients", true, "");
        show(&mut sh, &setup()).unwrap();
        assert!(sh.log.last().unwrap().contains("new tab in front window"));
    }

    #[test]
    fn shells_are_told_apart_from_the_command() {
        assert!(is_shell("zsh"));
        assert!(is_shell("-zsh"));
        assert!(is_shell("/bin/bash"));
        assert!(!is_shell("claude"));
        assert!(!is_shell("node"));
    }
}

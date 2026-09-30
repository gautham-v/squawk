//! `squawk` — the command line and TUI.
//!
//! Commands that read files (`last`, `history`, `meet list`, `dict`) work
//! without the app running. Commands that need the running app (`status`,
//! `meet start`, `meet stop`) go through the socket and say so plainly when
//! it is not running.

mod commands;
mod format;
mod system;
mod tui;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "squawk",
    version,
    about = "Local dictation for your Mac. No args opens the TUI."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the last dictation.
    Last,
    /// Print recent dictations, newest first.
    History {
        /// Only today's.
        #[arg(long)]
        today: bool,
        /// How many (default 20).
        #[arg(short = 'n', long = "n", default_value_t = 20)]
        n: usize,
        /// One JSON object per line instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Meetings: start, stop, list.
    Meet {
        #[command(subcommand)]
        command: MeetCommand,
    },
    /// What the app is doing: state, model, permissions, meeting.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Make the running app re-read config.toml.
    Reload,
    /// The dictionary (~/squawk/dictionary.txt).
    Dict {
        #[command(subcommand)]
        command: DictCommand,
    },
    /// The speech model.
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
    /// Transcribe an audio file offline and print the cleaned text.
    Transcribe {
        file: std::path::PathBuf,
        /// Print the raw model text too, and timings.
        #[arg(long)]
        raw: bool,
        /// Apply Claude Code mode as if a session were running in this
        /// directory (to test @mentions and repo vocabulary offline).
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum MeetCommand {
    /// Start recording a meeting (the app must be running).
    Start {
        #[arg(long)]
        title: Option<String>,
    },
    /// Stop the meeting and print the file path.
    Stop,
    /// List meetings, newest first.
    List {
        #[arg(short = 'n', long = "n", default_value_t = 20)]
        n: usize,
    },
}

#[derive(Debug, Subcommand)]
enum DictCommand {
    /// Add a term ("Kubernetes") or a replacement ("cloud code -> Claude Code").
    Add {
        /// The entry; quote it or pass it as several words.
        #[arg(required = true, num_args = 1.., allow_hyphen_values = true, trailing_var_arg = true)]
        phrase: Vec<String>,
    },
    /// Print every entry.
    List,
}

#[derive(Debug, Subcommand)]
enum ModelCommand {
    /// Download the model if it is missing (~480 MB, once).
    Download,
}

fn main() {
    let cli = Cli::parse();
    let code = match run(cli) {
        Ok(()) => 0,
        Err(e) => match e.downcast::<commands::Exit>() {
            Ok(exit) => {
                eprintln!("{}", exit.message);
                exit.code
            }
            Err(e) => {
                eprintln!("squawk: {}", message(&e));
                1
            }
        },
    };
    std::process::exit(code);
}

/// The error and its causes, "a: b: c", without the repeats a transparent
/// wrapper adds (an io error wrapped once shows its text twice otherwise).
fn message(e: &anyhow::Error) -> String {
    let mut parts: Vec<String> = Vec::new();
    for cause in e.chain() {
        let text = cause.to_string();
        if parts.last().is_none_or(|p| !p.ends_with(&text)) {
            parts.push(text);
        }
    }
    parts.join(": ")
}

fn run(cli: Cli) -> anyhow::Result<()> {
    let env = commands::Env::load()?;
    match cli.command {
        None => tui::run(&env),
        Some(Command::Last) => commands::last(&env),
        Some(Command::History { today, n, json }) => commands::history(&env, today, n, json),
        Some(Command::Meet { command }) => match command {
            MeetCommand::Start { title } => commands::meet_start(&env, title),
            MeetCommand::Stop => commands::meet_stop(&env),
            MeetCommand::List { n } => commands::meet_list(&env, n),
        },
        Some(Command::Status { json }) => commands::status(&env, json),
        Some(Command::Reload) => commands::reload(&env),
        Some(Command::Dict { command }) => match command {
            DictCommand::Add { phrase } => commands::dict_add(&env, &phrase.join(" ")),
            DictCommand::List => commands::dict_list(&env),
        },
        Some(Command::Model { command }) => match command {
            ModelCommand::Download => commands::model_download(&env),
        },
        Some(Command::Transcribe { file, raw, cwd }) => {
            commands::transcribe(&env, &file, raw, cwd.as_deref())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn error_chain_without_repeats() {
        let io = std::io::Error::other("disk full");
        let e = anyhow::Error::new(squawk_core::Error::Io(io)).context("writing the day file");
        assert_eq!(message(&e), "writing the day file: disk full");
    }

    #[test]
    fn cli_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_the_documented_forms() {
        let parse = |args: &[&str]| {
            Cli::try_parse_from(std::iter::once("squawk").chain(args.iter().copied()))
        };
        assert!(parse(&[]).unwrap().command.is_none());
        assert!(matches!(
            parse(&["history", "--today", "--n", "5"]).unwrap().command,
            Some(Command::History {
                today: true,
                n: 5,
                ..
            })
        ));
        assert!(matches!(
            parse(&["meet", "start", "--title", "Standup"])
                .unwrap()
                .command,
            Some(Command::Meet {
                command: MeetCommand::Start { title: Some(_) }
            })
        ));
        assert!(matches!(
            parse(&["dict", "add", "cloud", "code", "->", "Claude", "Code"])
                .unwrap()
                .command,
            Some(Command::Dict {
                command: DictCommand::Add { .. }
            })
        ));
        assert!(parse(&["transcribe", "a.wav"]).is_ok());
        assert!(parse(&["transcribe", "a.wav", "--raw", "--cwd", "."]).is_ok());
        assert!(parse(&["model", "download"]).is_ok());
        assert!(parse(&["status", "--json"]).is_ok());
    }
}

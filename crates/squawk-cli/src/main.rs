//! `squawk` — the command line and TUI.
//!
//! Commands that read files (`last`, `history`, `meet list`, `dict`) work
//! without the app running. Commands that need the running app (`status`,
//! `meet start`, `meet stop`) go through the socket and say so plainly when
//! it is not running. `agent <event>` is the exception: the agent session's
//! hooks run it, so it is quiet and succeeds whether or not the app is there.

mod commands;
mod format;
mod system;
mod tui;

use clap::{Parser, Subcommand};
use squawk_core::ipc::AgentEvent;

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
    /// Transcribe an audio file offline and print the cleaned text (with
    /// S1-mini when `cleanup_model` is on and it is downloaded).
    Transcribe {
        file: std::path::PathBuf,
        /// Print the raw model text and S1-mini's answer too, and timings.
        #[arg(long)]
        raw: bool,
        /// Apply Claude Code mode as if a session were running in this
        /// directory (to test @mentions and repo vocabulary offline).
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
    },
    /// The agent (⌥+fn): its hooks report here, and `send` types into it.
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
}

#[derive(Debug, Subcommand)]
enum AgentCommand {
    /// The agent started on a request (its UserPromptSubmit hook).
    Start,
    /// The agent finished (its Stop hook): the menu bar goes back to rest.
    Stop,
    /// The agent is waiting on you (its Notification hook): show its window.
    Question,
    /// Show the agent's terminal window.
    Show,
    /// Type text into the agent's tmux session and press Enter, as an ⌥+fn
    /// dictation would (launching the session if it is not running).
    Send {
        #[arg(required = true, num_args = 1.., allow_hyphen_values = true, trailing_var_arg = true)]
        text: Vec<String>,
    },
}

impl AgentCommand {
    /// The hook event this is, or `None` for `send`.
    fn event(&self) -> Option<AgentEvent> {
        match self {
            AgentCommand::Start => Some(AgentEvent::Start),
            AgentCommand::Stop => Some(AgentEvent::Stop),
            AgentCommand::Question => Some(AgentEvent::Question),
            AgentCommand::Show => Some(AgentEvent::Show),
            AgentCommand::Send { .. } => None,
        }
    }
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
    /// Download the models if they are missing, once: Parakeet (~480 MB)
    /// and, while `cleanup_model` is on, S1-mini by Superwhisper (462 MB).
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
    if let Some(Command::Agent { command }) = &cli.command {
        if let Some(event) = command.event() {
            // A hook: no error of any kind (not even a missing HOME) may
            // fail it or print.
            if let Ok(env) = commands::Env::load() {
                let _ = commands::agent_event(&env, event);
            }
            return Ok(());
        }
    }
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
        Some(Command::Agent { command }) => match command.event() {
            Some(event) => commands::agent_event(&env, event),
            None => match command {
                AgentCommand::Send { text } => commands::agent_send(&env, &text.join(" ")),
                _ => Ok(()),
            },
        },
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
        for event in ["start", "stop", "question", "show"] {
            assert!(parse(&["agent", event]).is_ok(), "{event}");
        }
        assert!(matches!(
            parse(&["agent", "send", "open", "my", "notes"])
                .unwrap()
                .command,
            Some(Command::Agent {
                command: AgentCommand::Send { .. }
            })
        ));
        assert!(parse(&["agent", "send", "--verbose please"]).is_ok());
        assert!(parse(&["agent", "send"]).is_err());
        assert!(parse(&["agent", "nope"]).is_err());
    }
}

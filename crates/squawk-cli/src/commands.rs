//! Every subcommand but the TUI. Output is plain text for people (and for
//! Claude reading it); `--json` where a script would want it.

use std::path::Path;

use squawk_core::{Config, Paths, Store};

/// Paths, config and store, resolved once per run.
#[allow(dead_code)] // scaffold: the handlers below read these once implemented
pub struct Env {
    pub paths: Paths,
    pub config: Config,
    /// A malformed config file; printed as a warning by commands that care.
    pub config_note: Option<String>,
    pub store: Store,
}

impl Env {
    pub fn load() -> anyhow::Result<Env> {
        let paths = Paths::detect()?;
        let (config, config_note) = Config::load(&paths.config_file);
        let paths = paths.with_config(&config);
        let store = Store::new(&paths);
        Ok(Env {
            paths,
            config,
            config_note,
            store,
        })
    }
}

pub fn last(env: &Env) -> anyhow::Result<()> {
    let _ = env;
    todo!("cli agent")
}

pub fn history(env: &Env, today: bool, n: usize, json: bool) -> anyhow::Result<()> {
    let _ = (env, today, n, json);
    todo!("cli agent")
}

pub fn meet_start(env: &Env, title: Option<String>) -> anyhow::Result<()> {
    let _ = (env, title);
    todo!("cli agent")
}

pub fn meet_stop(env: &Env) -> anyhow::Result<()> {
    let _ = env;
    todo!("cli agent")
}

pub fn meet_list(env: &Env, n: usize) -> anyhow::Result<()> {
    let _ = (env, n);
    todo!("cli agent")
}

pub fn status(env: &Env, json: bool) -> anyhow::Result<()> {
    let _ = (env, json);
    todo!("cli agent")
}

pub fn dict_add(env: &Env, phrase: &str) -> anyhow::Result<()> {
    let _ = (env, phrase);
    todo!("cli agent")
}

pub fn dict_list(env: &Env) -> anyhow::Result<()> {
    let _ = env;
    todo!("cli agent")
}

pub fn model_download(env: &Env) -> anyhow::Result<()> {
    let _ = env;
    todo!("cli agent")
}

pub fn transcribe(env: &Env, file: &Path, raw: bool, cwd: Option<&Path>) -> anyhow::Result<()> {
    let _ = (env, file, raw, cwd);
    todo!("cli agent")
}

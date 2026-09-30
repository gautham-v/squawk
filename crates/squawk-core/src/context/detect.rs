//! Which coding agent is behind the front terminal, and where it runs.
//!
//! Walk the process tree down from the terminal's pid (Ghostty → login → zsh
//! → claude), collecting `claude`/`codex` processes and not descending into
//! them (their MCP servers and subagents are not sessions). With several
//! tabs running agents, the one whose tty was *read* most recently wins: the
//! tty's atime moves when you type into it, while an agent streaming output
//! in a background tab only moves its mtime.
//!
//! libproc and sysctl only, no subprocesses: a full pass is ~1–3 ms.
//!
//! Testing by hand (the process table can't be faked in CI): open two
//! Ghostty tabs, run `claude` in different repos, type in one of them, then
//! within a few seconds run
//! `cargo test -p squawk-core detect_live -- --ignored --nocapture`
//! from a third app (it prints the session it picked and how long it took;
//! the tab you typed in last should win).

use std::time::SystemTime;

use super::{is_terminal, Agent, FrontApp, Session, AGENT_PROCESS_NAMES};

/// One row of the process table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Proc {
    pub pid: i32,
    pub ppid: i32,
    /// Short name (`claude`, `node`, `2.1.3` for a versioned binary).
    pub name: String,
}

/// Find the agent session behind the front terminal: a `claude`/`codex`
/// process descended from `front.pid`; if several, the one whose controlling
/// tty was used most recently. `None` if the front app is not a terminal or
/// runs no agent.
pub fn detect(front: &FrontApp) -> Option<Session> {
    if !is_terminal(&front.bundle_id) {
        return None;
    }
    let table = sys::process_table();
    let agents = agents_under(front.pid, &table, |p| classify(p, sys::command_line));
    let mut best: Option<(Option<SystemTime>, Session)> = None;
    for (proc, agent) in agents {
        let Some(cwd) = sys::cwd(proc.pid) else {
            continue;
        };
        let tty = sys::tty_dev(proc.pid).and_then(sys::tty_path);
        let used = tty.as_deref().and_then(sys::tty_last_used);
        let session = Session {
            agent,
            pid: proc.pid,
            cwd,
            tty,
        };
        // Ties (no tty, same second) go to the newer process.
        let better = match &best {
            None => true,
            Some((t, s)) => (used, session.pid) > (*t, s.pid),
        };
        if better {
            best = Some((used, session));
        }
    }
    best.map(|(_, s)| s)
}

/// Agents descended from `root`, not looking inside an agent's own subtree.
pub(crate) fn agents_under(
    root: i32,
    table: &[Proc],
    classify: impl Fn(&Proc) -> Option<Agent>,
) -> Vec<(&Proc, Agent)> {
    let mut children: std::collections::HashMap<i32, Vec<&Proc>> = Default::default();
    for p in table {
        if p.pid != p.ppid {
            children.entry(p.ppid).or_default().push(p);
        }
    }
    let mut found = Vec::new();
    let mut stack = vec![root];
    let mut seen = std::collections::HashSet::from([root]);
    while let Some(pid) = stack.pop() {
        for &child in children.get(&pid).map(Vec::as_slice).unwrap_or(&[]) {
            if !seen.insert(child.pid) {
                continue;
            }
            match classify(child) {
                Some(agent) => found.push((child, agent)),
                None => stack.push(child.pid),
            }
        }
    }
    found.sort_by_key(|(p, _)| p.pid);
    found
}

/// Is this process an agent? The short name usually says (`claude`); a
/// versioned native install (`~/.local/share/claude/versions/2.1.3`) or a
/// node-hosted one needs the command line, which `command_line` reads
/// (exec path, argv).
pub(crate) fn classify(
    proc: &Proc,
    command_line: impl Fn(i32) -> Option<(String, Vec<String>)>,
) -> Option<Agent> {
    if let Some(agent) = agent_named(&proc.name) {
        return Some(agent);
    }
    if is_boring(&proc.name) {
        return None;
    }
    let (exe, argv) = command_line(proc.pid)?;
    classify_command(&exe, &argv)
}

fn agent_named(name: &str) -> Option<Agent> {
    AGENT_PROCESS_NAMES
        .iter()
        .find(|(n, _)| name.eq_ignore_ascii_case(n))
        .map(|(_, a)| *a)
}

/// Shells and the like: never an agent, not worth a sysctl.
fn is_boring(name: &str) -> bool {
    matches!(
        name,
        "login"
            | "zsh"
            | "-zsh"
            | "bash"
            | "-bash"
            | "fish"
            | "sh"
            | "nu"
            | "tmux"
            | "ssh"
            | "git"
            | "vim"
            | "nvim"
            | "less"
            | "man"
            | "cargo"
            | "rustc"
            | "rust-analyzer"
    )
}

/// The command-line half of [`classify`].
pub(crate) fn classify_command(exe: &str, argv: &[String]) -> Option<Agent> {
    let base = |s: &str| s.rsplit('/').next().unwrap_or(s).to_string();
    // `claude` launched through a symlink to a versioned binary.
    if let Some(agent) = argv.first().and_then(|a| agent_named(&base(a))) {
        return Some(agent);
    }
    // The versioned native binary, but not when it runs as one of its
    // multi-call tools (argv[0] = "ugrep").
    let argv0 = argv.first().map(|a| base(a));
    if exe.contains("/claude/versions/") && argv0.as_deref().is_none_or(|a| a == base(exe)) {
        return Some(Agent::Claude);
    }
    if let Some(agent) = agent_named(&base(exe)) {
        return Some(agent);
    }
    // node/bun running the npm package's script.
    let runtime = base(argv.first().map(String::as_str).unwrap_or(exe));
    if matches!(runtime.as_str(), "node" | "bun" | "deno")
        || matches!(base(exe).as_str(), "node" | "bun")
    {
        let script = argv.iter().skip(1).find(|a| !a.starts_with('-'))?;
        if script.contains("@anthropic-ai/claude-code") || script.contains("/claude-code/") {
            return Some(Agent::Claude);
        }
        if script.contains("@openai/codex") {
            return Some(Agent::Codex);
        }
        let name = base(script);
        let name = name
            .strip_suffix(".js")
            .or_else(|| name.strip_suffix(".mjs"))
            .unwrap_or(&name);
        return agent_named(name);
    }
    None
}

#[cfg(target_os = "macos")]
mod sys {
    //! The libproc / sysctl calls. Everything here fails soft: a process that
    //! exits mid-walk, or belongs to another user, is just skipped.

    use std::ffi::{c_char, c_int, c_void, CStr};
    use std::mem::{size_of, MaybeUninit};
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::Proc;

    extern "C" {
        // Thread-safe devname(3); not in the libc crate.
        fn devname_r(
            dev: libc::dev_t,
            kind: libc::mode_t,
            buf: *mut c_char,
            len: c_int,
        ) -> *mut c_char;
    }

    /// Every process, by the short BSD info: the one flavour macOS hands out
    /// for other users' processes too (`login`, between the terminal and
    /// the shell, runs as root).
    pub fn process_table() -> Vec<Proc> {
        list_pids().into_iter().filter_map(short_info).collect()
    }

    fn list_pids() -> Vec<i32> {
        // SAFETY: a null buffer asks for the count only.
        let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if n <= 0 {
            return Vec::new();
        }
        // Room for processes started between the two calls.
        let mut pids = vec![0i32; n as usize + 64];
        let bytes = (pids.len() * size_of::<i32>()) as c_int;
        // SAFETY: the buffer is `bytes` long and i32-aligned.
        let got = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast::<c_void>(), bytes) };
        if got <= 0 {
            return Vec::new();
        }
        pids.truncate(got as usize);
        pids.retain(|&p| p > 0);
        pids
    }

    /// `proc_pidinfo` into a zeroed `T`, if the kernel filled all of it.
    fn pidinfo<T>(pid: i32, flavor: c_int) -> Option<T> {
        let mut info = MaybeUninit::<T>::zeroed();
        let size = size_of::<T>() as c_int;
        // SAFETY: `info` is a zeroed T of exactly `size` bytes; the flavours
        // used here are plain C structs for which all-zero is valid.
        let got =
            unsafe { libc::proc_pidinfo(pid, flavor, 0, info.as_mut_ptr().cast::<c_void>(), size) };
        // SAFETY: as above.
        (got == size).then(|| unsafe { info.assume_init() })
    }

    fn short_info(pid: i32) -> Option<Proc> {
        let info: libc::proc_bsdshortinfo = pidinfo(pid, libc::PROC_PIDT_SHORTBSDINFO)?;
        Some(Proc {
            pid: info.pbsi_pid as i32,
            ppid: info.pbsi_ppid as i32,
            name: c_chars(&info.pbsi_comm),
        })
    }

    /// The controlling tty device (same-user processes only, which agents are).
    pub fn tty_dev(pid: i32) -> Option<u32> {
        let info: libc::proc_bsdinfo = pidinfo(pid, libc::PROC_PIDTBSDINFO)?;
        // NODEV is all ones.
        (info.e_tdev != u32::MAX && info.e_tdev != 0).then_some(info.e_tdev)
    }

    fn c_chars(chars: &[c_char]) -> String {
        let bytes: Vec<u8> = chars
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    pub fn cwd(pid: i32) -> Option<PathBuf> {
        let info: libc::proc_vnodepathinfo = pidinfo(pid, libc::PROC_PIDVNODEPATHINFO)?;
        let raw = &info.pvi_cdir.vip_path;
        // SAFETY: [[c_char; 32]; 32] is 1024 contiguous bytes.
        let bytes = unsafe { std::slice::from_raw_parts(raw.as_ptr().cast::<u8>(), 32 * 32) };
        let path = CStr::from_bytes_until_nul(bytes).ok()?.to_str().ok()?;
        (!path.is_empty()).then(|| PathBuf::from(path))
    }

    /// Exec path and argv, from `KERN_PROCARGS2`.
    pub fn command_line(pid: i32) -> Option<(String, Vec<String>)> {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut buf = vec![0u8; arg_max()];
        let mut len = buf.len();
        // SAFETY: `buf` is `len` bytes; the kernel writes at most that.
        let r = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buf.as_mut_ptr().cast::<c_void>(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if r != 0 || len < size_of::<c_int>() {
            return None;
        }
        parse_procargs(&buf[..len])
    }

    /// `argc`, exec path, NUL padding, then `argc` NUL-terminated args.
    pub(super) fn parse_procargs(buf: &[u8]) -> Option<(String, Vec<String>)> {
        let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
        let rest = &buf[4..];
        let exe_end = rest.iter().position(|&b| b == 0)?;
        let exe = String::from_utf8_lossy(&rest[..exe_end]).into_owned();
        let mut args = Vec::new();
        let mut parts = rest[exe_end..]
            .split(|&b| b == 0)
            .skip_while(|s| s.is_empty());
        for _ in 0..argc.max(0) {
            let Some(arg) = parts.next() else { break };
            args.push(String::from_utf8_lossy(arg).into_owned());
        }
        Some((exe, args))
    }

    fn arg_max() -> usize {
        static MAX: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        *MAX.get_or_init(|| {
            let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
            let mut value: c_int = 0;
            let mut len = size_of::<c_int>();
            // SAFETY: reading one c_int.
            let r = unsafe {
                libc::sysctl(
                    mib.as_mut_ptr(),
                    2,
                    (&mut value as *mut c_int).cast::<c_void>(),
                    &mut len,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if r == 0 && value > 0 {
                value as usize
            } else {
                1 << 20
            }
        })
    }

    pub fn tty_path(dev: u32) -> Option<PathBuf> {
        let mut buf = [0 as c_char; 64];
        // SAFETY: `buf` is 64 bytes; devname_r writes a NUL-terminated name.
        let p = unsafe {
            devname_r(
                dev as libc::dev_t,
                libc::S_IFCHR,
                buf.as_mut_ptr(),
                buf.len() as c_int,
            )
        };
        if p.is_null() {
            return None;
        }
        // SAFETY: devname_r returned a pointer to a NUL-terminated string.
        let name = unsafe { CStr::from_ptr(p) }.to_str().ok()?;
        (!name.is_empty() && !name.starts_with('?')).then(|| Path::new("/dev").join(name))
    }

    /// When the tty was last read from (typed into), falling back to written.
    pub fn tty_last_used(tty: &Path) -> Option<SystemTime> {
        let m = std::fs::metadata(tty).ok()?;
        let at = |s: i64, ns: i64| {
            UNIX_EPOCH + Duration::new(s.max(0) as u64, ns.clamp(0, 999_999_999) as u32)
        };
        let read = at(m.atime(), m.atime_nsec());
        let written = at(m.mtime(), m.mtime_nsec());
        Some(if read > UNIX_EPOCH { read } else { written })
    }
}

#[cfg(not(target_os = "macos"))]
mod sys {
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    use super::Proc;

    pub fn process_table() -> Vec<Proc> {
        Vec::new()
    }
    pub fn tty_dev(_pid: i32) -> Option<u32> {
        None
    }
    pub fn cwd(_pid: i32) -> Option<PathBuf> {
        None
    }
    pub fn command_line(_pid: i32) -> Option<(String, Vec<String>)> {
        None
    }
    pub fn tty_path(_dev: u32) -> Option<PathBuf> {
        None
    }
    pub fn tty_last_used(_tty: &Path) -> Option<SystemTime> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: i32, ppid: i32, name: &str) -> Proc {
        Proc {
            pid,
            ppid,
            name: name.to_string(),
        }
    }

    fn by_name(proc: &Proc) -> Option<Agent> {
        classify(proc, |_| None)
    }

    #[test]
    fn finds_agents_under_the_terminal_only() {
        let table = vec![
            p(100, 1, "ghostty"),
            p(101, 100, "login"),
            p(102, 101, "zsh"),
            p(103, 102, "claude"),
            p(104, 103, "node"),   // an MCP server under claude
            p(105, 103, "claude"), // a subagent: not a separate session
            p(111, 100, "login"),
            p(112, 111, "zsh"),
            p(113, 112, "codex"),
            p(200, 1, "iTerm2"),
            p(201, 200, "claude"),
        ];
        let found: Vec<(i32, Agent)> = agents_under(100, &table, by_name)
            .into_iter()
            .map(|(p, a)| (p.pid, a))
            .collect();
        assert_eq!(found, vec![(103, Agent::Claude), (113, Agent::Codex)]);
    }

    #[test]
    fn survives_cycles_and_self_parents() {
        let table = vec![p(1, 1, "launchd"), p(5, 6, "a"), p(6, 5, "b")];
        assert!(agents_under(5, &table, by_name).is_empty());
    }

    #[test]
    fn classifies_by_command_line() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let native = "/Users/you/.local/share/claude/versions/2.1.3";
        assert_eq!(
            classify_command(native, &s(&["claude"])),
            Some(Agent::Claude)
        );
        assert_eq!(classify_command(native, &s(&[native])), Some(Agent::Claude));
        assert_eq!(classify_command(native, &s(&["ugrep", "-G"])), None);
        assert_eq!(
            classify_command(
                "/opt/homebrew/bin/node",
                &s(&[
                    "node",
                    "--no-warnings",
                    "/opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/cli.js"
                ])
            ),
            Some(Agent::Claude)
        );
        assert_eq!(
            classify_command("/usr/local/bin/node", &s(&["node", "/usr/local/bin/codex"])),
            Some(Agent::Codex)
        );
        assert_eq!(
            classify_command("/usr/local/bin/node", &s(&["node", "server.js"])),
            None
        );
        assert_eq!(
            classify_command("/usr/bin/vim", &s(&["vim", "CLAUDE.md"])),
            None
        );
    }

    #[test]
    fn versioned_binary_needs_the_command_line() {
        let proc = p(7, 1, "2.1.3");
        let got = classify(&proc, |_| {
            Some((
                "/Users/you/.local/share/claude/versions/2.1.3".into(),
                vec!["claude".into()],
            ))
        });
        assert_eq!(got, Some(Agent::Claude));
        // Shells never cost a sysctl.
        let zsh = p(8, 1, "zsh");
        assert_eq!(classify(&zsh, |_| panic!("no lookup for a shell")), None);
    }

    #[test]
    fn not_a_terminal_is_none() {
        let front = FrontApp {
            bundle_id: "com.apple.Safari".into(),
            name: "Safari".into(),
            pid: 1,
        };
        assert_eq!(detect(&front), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reads_this_process() {
        let me = std::process::id() as i32;
        let table = sys::process_table();
        let row = table.iter().find(|p| p.pid == me).expect("own pid listed");
        assert_eq!(row.ppid, std::os::unix::process::parent_id() as i32);
        assert_eq!(sys::cwd(me), std::env::current_dir().ok());
        let (exe, argv) = sys::command_line(me).expect("own argv");
        assert!(exe.contains("squawk_core"), "{exe}");
        assert!(argv[0].contains("squawk_core"), "{argv:?}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parses_procargs() {
        let mut buf = 2i32.to_ne_bytes().to_vec();
        buf.extend(b"/bin/claude\0\0\0\0claude\0--resume\0PATH=/bin\0");
        assert_eq!(
            sys::parse_procargs(&buf),
            Some((
                "/bin/claude".into(),
                vec!["claude".into(), "--resume".into()]
            ))
        );
        assert_eq!(sys::parse_procargs(&[1, 0]), None);
    }

    /// See the module docs. Picks the frontmost-ish Ghostty and prints.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn detect_live() {
        let table = sys::process_table();
        let Some(term) = table
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case("ghostty"))
        else {
            eprintln!("no Ghostty running");
            return;
        };
        let front = FrontApp {
            bundle_id: "com.mitchellh.ghostty".into(),
            name: "Ghostty".into(),
            pid: term.pid,
        };
        for (p, agent) in agents_under(term.pid, &table, |p| classify(p, sys::command_line)) {
            let tty = sys::tty_dev(p.pid).and_then(sys::tty_path);
            let used = tty.as_deref().and_then(sys::tty_last_used);
            eprintln!(
                "candidate {} {agent:?} {tty:?} {used:?} {:?}",
                p.pid,
                sys::cwd(p.pid)
            );
        }
        let t = std::time::Instant::now();
        let session = detect(&front);
        eprintln!("detect took {:?}: {session:#?}", t.elapsed());
    }
}

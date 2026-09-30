# squawk

Local dictation for macOS. Hold fn, talk, let go: the words land in whatever has focus.

Built for talking to Claude Code in a terminal. When the front app is a terminal running `claude`
(or `codex`), spoken file names become `@mentions` ("look at audio dot rs" → `look at
@src/audio.rs`) and the repo's jargon comes out spelled the way the repo spells it. Meetings are
recorded as two tracks, you and them, and transcribed while they happen.

Everything runs on your Mac with NVIDIA's Parakeet TDT 0.6B v3. No account, no API keys, no
telemetry. The network is used once, to download the model.

Apple Silicon, macOS 15 Sequoia.

## Install

```sh
make install
```

That builds `Squawk.app` into `/Applications`, installs the `squawk` CLI into `~/.local/bin`, and
opens the app. It lives in the menu bar: five small bars.

`~/.local/bin` is not on a stock macOS `PATH`. Add it (`export PATH="$HOME/.local/bin:$PATH"` in
`~/.zshrc`), or install the CLI somewhere that is: `make install BIN_DIR=/usr/local/bin`.

Or by hand, if you would rather put the CLI in `~/.cargo/bin`:

```sh
make bundle                                   # builds and signs Squawk.app
cp -R target/Squawk.app /Applications/
cargo install --locked --path crates/squawk-cli   # the `squawk` CLI
```

Needs a Rust toolchain (`rustup`) and the Xcode command line tools. The build signs the app with a
codesigning identity from your keychain (a Developer ID if you have one, else the first identity
`security find-identity -v -p codesigning` lists, such as an Apple Development certificate), so
macOS keeps its permissions across rebuilds. Pick one with `CODESIGN_IDENTITY="<name>" make
install`. With no identity at all it signs ad-hoc, and macOS asks for permissions again after
every build.

## First run

1. **Accessibility.** macOS asks once; turn Squawk on in System Settings › Privacy & Security ›
   Accessibility. Squawk notices within a couple of seconds, no relaunch needed. (It needs this to
   see the fn key and to paste.)
2. **Microphone.** Asked the first time you dictate.
3. **The model** (~480 MB) downloads on first launch. The menu bar popover shows the progress.
4. **System Settings › Keyboard › "Press 🌐 key to" = "Do nothing".** Otherwise every fn press also
   opens the emoji picker or switches your input source. The popover reminds you until it is set.
5. **Anything else on fn.** Wispr Flow, Superwhisper, macOS Dictation and the like listen to the
   same key. Quit them, or move their shortcut, before you start Squawk, or both will record.

Screen & System Audio Recording is only needed for meetings, and is asked for the first time you
record one.

## Using it

| gesture | does |
|---|---|
| hold fn, talk, release | push to talk: pastes when you let go |
| double-tap fn | hands-free: keeps listening after you let go; press fn once more to paste |
| a single quick tap | nothing (discarded) |
| fn with another key (fn+arrow, fn+F5…) | nothing: you were using fn as a modifier |
| esc while recording | cancel |
| ctrl+cmd+V | paste the last dictation again |
| ctrl+cmd+C | copy the last dictation |
| ⌥M | start or stop a meeting |

The menu bar glyph turns copper with a timer while it listens (with a small lock when hands-free),
dims briefly while it transcribes, and shows `● 12:04` during a meeting.

Squawk pastes through the clipboard and puts your old clipboard back a moment later. It never
presses return: in a terminal the text waits for you to send it.

Text is cleaned up without a language model: fillers (um, uh, a comma-wrapped "you know", "like",
"kind of"…) and stutters go, the first letter is capitalised, a full stop is added. Words that
mean something stay ("I like Rust" keeps its "like").

The popover (click the menu bar item) has your recent dictations (click one to copy it),
meetings (click to open), and your dictionary.

## Speed

Parakeet runs on the CPU at roughly 25× real time on an M4. Squawk transcribes while you talk,
cutting at pauses, and starts on the last stretch a fifth of a second after you stop, so letting
go of fn leaves little to do. Measured on an M4 (release build, `say` speech replayed in real time
through the streaming path; `cargo test --release -p squawk-engine --test end_to_end -- --ignored
--nocapture`):

| dictation | release a beat after the last word | release on the last word |
|---|---|---|
| 6 s | 30–70 ms | |
| 21 s | ~50 ms | ~250 ms |

Cleanup and Claude Code mode add well under a millisecond. The model loads in under a second at
launch. The log line for each dictation has `release_to_paste_ms` if you want your own numbers.

## Claude Code mode

When you dictate into Ghostty, Terminal, iTerm2, WezTerm, Warp, kitty or Alacritty and a `claude`
or `codex` session is running in that window, squawk finds the session's working directory and:

- turns spoken file references into `@mentions` when the match is unambiguous: "audio dot rs" →
  `@src/audio.rs`, "the claude md" → `@CLAUDE.md`, "cargo toml" → `@Cargo.toml`, "source slash
  main dot rs" → `@src/main.rs`;
- spells the repo's own words its way (crate and package names, file names, terms from README.md,
  CLAUDE.md, AGENTS.md, CONTEXT.md).

References that could mean more than one file (two `main.rs`) are left as spoken; say more of
the path ("squawk core slash lib dot rs") to pick one. With several `claude` tabs, the one you
typed in last wins. Sessions inside tmux are not detected yet; you get plain dictation there.

Try it offline against any repo: `squawk transcribe clip.m4a --raw --cwd ~/code/project`.

Turn it off by setting `claude_code_mode = false` under `[dictation]` in
`~/.config/squawk/config.toml`.

## Files

Plain files, so you (and Claude) can read them directly.

| path | what |
|---|---|
| `~/squawk/dictations/YYYY-MM-DD.md` | every dictation of the day, `## 14:03:12 · Ghostty · squawk` then the text |
| `~/squawk/meetings/YYYY-MM-DD HHMM <title>.md` | one file per meeting: front matter, then `**You** 00:23:06` / `**Them**` blocks |
| `~/squawk/dictionary.txt` | your words, one per line: `Kubernetes`, or `cloud code -> Claude Code` |
| `~/.config/squawk/config.toml` | settings (every key commented with its default; edit a key under its existing `[section]`) |
| `~/Library/Application Support/squawk/` | the model, the socket, `squawk.log` |

The log has one line per dictation with its timings (never its text), e.g. `audio=20.4s
segments=5 tail_ms=182 … release_to_paste_ms=197`.

## The CLI

```sh
squawk                       # a TUI over history and meetings
squawk last                  # the last dictation (squawk last | pbcopy)
squawk history [--today] [-n N] [--json]
squawk meet start [--title T] | meet stop | meet list
squawk status [--json]
squawk reload                # re-read config.toml (opening the popover does too)
squawk dict add <phrase>     # "Kubernetes" or "cloud code -> Claude Code"
squawk dict list
squawk model download
squawk transcribe <audio file> [--raw] [--cwd DIR]
```

`status`, `reload` and `meet start/stop` talk to the running app; everything else reads the files
and works without it.

The TUI follows your terminal's colours. History and Meetings side by side with a preview:

| key | does |
|---|---|
| `↑↓` / `j k`, `g G` | move, top/bottom |
| `/` | search (every word must match); `enter` keeps it, `esc` clears |
| `enter` | copy the dictation, or the whole meeting transcript |
| `o` | open the meeting (or that day's dictations) in `$EDITOR` |
| `tab` | switch between History and Meetings |
| `ctrl-d` / `ctrl-u` | scroll the preview |
| `r`, `q` | reload, quit |

It picks up new dictations and a growing meeting on its own.

## Meetings

⌥M (or "Record meeting" in the popover, or `squawk meet start`) records your mic as **You** and the
Mac's audio as **Them**, and transcribes both in 30-second chunks as the meeting goes, so the file
is readable while it grows. The title comes from the calendar event happening now, if Squawk may
read your calendar, else "Meeting".

System audio needs Screen & System Audio Recording.

Headphones are not needed. On speakers the other side also reaches your mic, so during a meeting
Squawk records the mic through Apple's voice processing (the echo cancellation built into
macOS for calls), which takes out whatever your Mac is playing, and it drops any "You" line that repeats what "Them"
said at the same moment. Short answers like "Yes." are always kept. While a meeting records, other
audio is ducked a little (about 8 dB, the least macOS allows). With headphones you can turn this
off: `echo_cancellation = false` under `[meeting]`. Dictation never uses it.

## Privacy

- Speech never leaves your Mac. The only network request is the one-time model download.
- Audio is not kept. Set `keep_audio = true` to keep WAVs for debugging.
- No telemetry, no accounts.

## Development

```sh
make run        # build Squawk.app and launch it
make test       # cargo test --workspace
make check      # fmt + clippy
make preview    # the popover with fixture data (MODE=recording|meeting|downloading|permissions|…)
cargo run -p squawk-app --example menu_bar_preview   # every menu bar state at once
```

The workspace: `squawk-core` (config, files, cleanup, the fn state machine, IPC, Claude Code
mode), `squawk-engine` (audio, Parakeet, streaming dictation, meetings), `squawk-app` (the menu
bar app), `squawk-cli` (`squawk`). [docs/design.md](docs/design.md) describes how they fit.

## Credits

- [Parakeet TDT 0.6B v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3) by NVIDIA.
- The ONNX export by [istupakov](https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx).
- [transcribe-rs](https://github.com/cjpais/transcribe-rs) and [Handy](https://github.com/cjpais/Handy)
  by cjpais, which showed the way and host the model download.
- [GPUI](https://www.gpui.rs/) from Zed for the popover, [ratatui](https://ratatui.rs/) for the TUI,
  [cpal](https://github.com/RustAudio/cpal) for the mic and
  [screencapturekit-rs](https://github.com/doom-fish/screencapturekit-rs) for system audio.

## License

MIT

#!/bin/bash
# Scaffold the agent's directory: where squawk starts the Claude Code session
# that ⌥+fn dictates to. The directory is [agent] dir in
# ~/.config/squawk/config.toml (default ~/agent), or the first argument.
#
# Writes CLAUDE.md and .claude/settings.json. Never overwrites a file that is
# already there: delete it first to get the template again.
#
# The hooks keep squawk's menu bar in step: a prompt submitted → `squawk agent
# start`, the turn over → `stop`, Claude waiting on you (a permission prompt,
# a question) → `question`, which brings the window forward. Notification's
# idle_prompt, sent a minute after every answer, is left out on purpose.
# Each hook ends in `|| true`: a Stop or UserPromptSubmit hook that exits 2
# (an older squawk without `agent`, say) would block Claude.
#
# Claude Code no longer honours defaultMode = bypassPermissions from a
# project's settings; it is here for older versions. To bypass for this
# session only, set [agent] command = "claude --permission-mode bypassPermissions".

set -euo pipefail

CONFIG="$HOME/.config/squawk/config.toml"

# [agent] dir from the config, if it sets one (a plain `dir = "..."` line).
config_dir() {
  [ -f "$CONFIG" ] || return 0
  awk '
    /^[[:space:]]*\[/ { in_agent = ($0 ~ /^[[:space:]]*\[agent\][[:space:]]*(#.*)?$/) ; next }
    in_agent && /^[[:space:]]*dir[[:space:]]*=/ {
      sub(/^[^=]*=[[:space:]]*/, "")
      if (match($0, /^"[^"]*"/)) { print substr($0, 2, RLENGTH - 2); exit }
      if (match($0, /^'"'"'[^'"'"']*'"'"'/)) { print substr($0, 2, RLENGTH - 2); exit }
    }
  ' "$CONFIG"
}

dir="${1:-$(config_dir)}"
dir="${dir:-~/agent}"
case "$dir" in
  "~") dir="$HOME" ;;
  "~/"*) dir="$HOME/${dir#\~/}" ;;
esac

mkdir -p "$dir/.claude"

# Write stdin to $1 unless it exists.
write_new() {
  if [ -e "$1" ] || [ -L "$1" ]; then
    echo "kept    $1 (already there)"
    cat > /dev/null
  else
    cat > "$1"
    echo "wrote   $1"
  fi
}

write_new "$dir/CLAUDE.md" <<'EOF'
# Agent

This is Gautham's voice-driven agent session on his Mac. He reaches it from anywhere through
Squawk, his dictation app: he holds ⌥+fn, says what he wants, and Squawk types it here and
presses return. He is usually in another app while you work, and may not be looking at this
window.

## How requests arrive

- One short spoken sentence, already transcribed and cleaned up. Expect the odd misheard word;
  read for the intent.
- "New conversation" clears the session; you will just see `/clear`.
- Act on the request. Do not ask for confirmation unless something is destructive or truly
  ambiguous.

## Finishing

- When he asks to open or show something, bring that app to the front as the last step:
  `open -a "Firefox Developer Edition"`, `open -a Catcher`, `open -a Finder ~/notes`, and so on.
- When the request is a question rather than an action, finish by running `squawk agent show` so
  this window comes forward and he sees the answer.
- Keep replies short: a sentence or two. He reads them at a glance, if at all.

## Where things are

- His notes live in `~/notes`.
- His dictations and meeting transcripts live in `~/squawk`: `dictations/YYYY-MM-DD.md` and
  `meetings/YYYY-MM-DD HHMM <title>.md`. Read them when he asks what he said or what a meeting
  covered.
- The browser is Firefox Developer Edition. Drive it with the Firefox agent bridge tools
  (`mcp__firefox__*`): open tabs, read pages, fill forms.
EOF

write_new "$dir/.claude/settings.json" <<'EOF'
{
  "permissions": {
    "defaultMode": "bypassPermissions"
  },
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          { "type": "command", "command": "\"$HOME/.local/bin/squawk\" agent start >/dev/null 2>&1 || true" }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          { "type": "command", "command": "\"$HOME/.local/bin/squawk\" agent stop >/dev/null 2>&1 || true" }
        ]
      }
    ],
    "Notification": [
      {
        "matcher": "permission_prompt|elicitation_dialog|agent_needs_input",
        "hooks": [
          { "type": "command", "command": "\"$HOME/.local/bin/squawk\" agent question >/dev/null 2>&1 || true" }
        ]
      }
    ],
    "PreToolUse": [
      {
        "matcher": "AskUserQuestion",
        "hooks": [
          { "type": "command", "command": "\"$HOME/.local/bin/squawk\" agent question >/dev/null 2>&1 || true" }
        ]
      }
    ]
  }
}
EOF

echo "agent directory: $dir"

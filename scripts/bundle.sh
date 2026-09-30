#!/usr/bin/env bash
# Build a release binary and assemble Squawk.app around it.
#
# The .app is not cosmetic: LSUIElement keeps squawk out of the Dock, the usage
# strings are what make macOS show the Microphone and Calendar prompts at all,
# and a stable signature is what keeps the Accessibility and Microphone grants
# across rebuilds.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Honour CARGO_TARGET_DIR / .cargo/config.toml rather than assuming ./target.
TARGET_DIR="$(cargo metadata --no-deps --format-version 1 --manifest-path "$ROOT/Cargo.toml" \
  | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
TARGET_DIR="${TARGET_DIR:-$ROOT/target}"

APP="$TARGET_DIR/Squawk.app"
# SQUAWK_BIN points at a prebuilt binary; otherwise build for this machine.
BIN="${SQUAWK_BIN:-$TARGET_DIR/release/squawk-app}"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)"

if [ -z "${SQUAWK_BIN:-}" ]; then
  cargo build --release -p squawk-app --manifest-path "$ROOT/Cargo.toml"
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/squawk-app"
cp "$ROOT/assets/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>Squawk</string>
	<key>CFBundleDisplayName</key>
	<string>Squawk</string>
	<key>CFBundleExecutable</key>
	<string>squawk-app</string>
	<key>CFBundleIdentifier</key>
	<string>com.gauthamv.squawk</string>
	<key>CFBundleIconFile</key>
	<string>AppIcon</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>${VERSION}</string>
	<key>CFBundleVersion</key>
	<string>1</string>
	<key>LSMinimumSystemVersion</key>
	<string>14.0</string>
	<key>LSUIElement</key>
	<true/>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSMicrophoneUsageDescription</key>
	<string>Squawk listens while you hold fn and turns your speech into text, on this Mac.</string>
	<key>NSAudioCaptureUsageDescription</key>
	<string>Squawk records the other side of a meeting you start, and transcribes it on this Mac.</string>
	<key>NSCalendarsFullAccessUsageDescription</key>
	<string>Squawk names meeting notes after the calendar event happening now.</string>
	<key>NSCalendarsUsageDescription</key>
	<string>Squawk names meeting notes after the calendar event happening now.</string>
</dict>
</plist>
PLIST

# Sign with a stable identity when the machine has one. An ad-hoc signature is
# a new code identity on every rebuild, so macOS forgets the Accessibility and
# Microphone grants each time. Override with CODESIGN_IDENTITY.
IDENTITY="${CODESIGN_IDENTITY:-}"
if [ -z "$IDENTITY" ]; then
  # Prefer Developer ID, which Gatekeeper trusts, over a development cert.
  IDENTITIES="$(security find-identity -v -p codesigning 2>/dev/null | sed -n 's/.*"\(.*\)"/\1/p')"
  IDENTITY="$(printf '%s\n' "$IDENTITIES" | grep -m1 '^Developer ID Application' || printf '%s\n' "$IDENTITIES" | head -n 1)"
fi
ENTITLEMENTS="$ROOT/scripts/entitlements.plist"
if [ -n "$IDENTITY" ]; then
  # Hardened runtime needs the audio-input entitlement, or the mic is
  # silently denied.
  codesign --force --options runtime --entitlements "$ENTITLEMENTS" --sign "$IDENTITY" "$APP"
else
  echo "note: no codesigning identity found; signing ad-hoc, so macOS will ask for permissions again after every rebuild"
  codesign --force --entitlements "$ENTITLEMENTS" --sign - "$APP" 2>/dev/null \
    || echo "warning: ad-hoc codesign failed"
fi

echo "built $APP"

# When CARGO_TARGET_DIR points elsewhere, keep ./target/Squawk.app working.
if [ "$TARGET_DIR" != "$ROOT/target" ]; then
  # A copy, not a symlink: Launch Services refuses to open a symlinked .app.
  mkdir -p "$ROOT/target"
  rm -rf "$ROOT/target/Squawk.app"
  cp -R "$APP" "$ROOT/target/Squawk.app"
  echo "copied to $ROOT/target/Squawk.app"
fi

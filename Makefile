.PHONY: run install install-cli test check bundle preview clean

TARGET_DIR := $(shell cargo metadata --no-deps --format-version 1 | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
BIN_DIR ?= $(HOME)/.local/bin

# Build the bundle and launch it (quits any running copy first).
run: bundle
	@pkill -x squawk-app 2>/dev/null || true
	open "$(TARGET_DIR)/Squawk.app"

# Put the app somewhere permanent and run it from there: launch at login
# registers whatever path the app was launched from, and the permission
# grants follow the signed app. Also installs the `squawk` CLI.
install: bundle install-cli
	@pkill -x squawk-app 2>/dev/null || true
	rm -rf "/Applications/Squawk.app"
	cp -R "$(TARGET_DIR)/Squawk.app" "/Applications/Squawk.app"
	open "/Applications/Squawk.app"

install-cli:
	cargo build --release -p squawk-cli
	mkdir -p "$(BIN_DIR)"
	install -m 755 "$(TARGET_DIR)/release/squawk" "$(BIN_DIR)/squawk"
	@echo "installed $(BIN_DIR)/squawk"

test:
	cargo test --workspace

check:
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings

bundle:
	./scripts/bundle.sh

# The popover with fixture data: make preview MODE=recording
preview:
	cargo run -p squawk-app --example popover_preview -- $(or $(MODE),ready)

clean:
	cargo clean

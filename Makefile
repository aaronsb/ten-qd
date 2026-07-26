.DEFAULT_GOAL := help
.PHONY: help deps build release run test lint fmt fmt-check check clean install uninstall forget screenshot radio-check

# Edition 2024.
MIN_RUST := 1.85
BIN      := target/release/ten-qd

# XDG user-scope install. Override with `make install PREFIX=/usr/local`.
PREFIX   ?= $(or $(XDG_DATA_HOME),$(HOME)/.local)
BINDIR   ?= $(PREFIX)/bin
STATEDIR := $(or $(XDG_STATE_HOME),$(HOME)/.local/state)/ten-qd

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-13s\033[0m %s\n", $$1, $$2}'
	@echo
	@echo "  Start with 'make deps' — it reports what is missing and how to get it."

# ---------------------------------------------------------------------------
# Dependencies
#
# Three hard requirements beyond Rust itself, all resolved through pkg-config
# at build time: alsa-lib (cpal's Linux backend), librtlsdr (the tuner), and
# pkg-config to find them. Everything else this checks is a runtime nicety and
# only warns.
# ---------------------------------------------------------------------------

deps: ## Check build and runtime dependencies
	@fail=0; warn=0; \
	ok()   { printf '  \033[32m✓\033[0m %s\n' "$$1"; }; \
	bad()  { printf '  \033[31m✗\033[0m %s\n' "$$1"; }; \
	note() { printf '  \033[33m!\033[0m %s\n' "$$1"; }; \
	\
	echo "toolchain"; \
	if command -v cargo >/dev/null 2>&1; then \
		v=$$(rustc --version 2>/dev/null | awk '{print $$2}'); \
		if [ "$$(printf '%s\n%s\n' "$(MIN_RUST)" "$$v" | sort -V | head -1)" = "$(MIN_RUST)" ]; then \
			ok "rust $$v"; \
		else \
			bad "rust $$v — edition 2024 needs $(MIN_RUST) or newer (rustup update)"; fail=1; \
		fi; \
	else \
		bad "cargo not found — https://rustup.rs"; fail=1; \
	fi; \
	if command -v cc >/dev/null 2>&1 || command -v gcc >/dev/null 2>&1; then \
		ok "c compiler"; else bad "no c compiler — needed to link the system libraries"; fail=1; fi; \
	if command -v pkg-config >/dev/null 2>&1; then \
		ok "pkg-config"; else bad "pkg-config not found — the build resolves both libraries through it"; fail=1; fi; \
	\
	echo; echo "libraries"; \
	for lib in alsa librtlsdr; do \
		if pkg-config --exists $$lib 2>/dev/null; then \
			ok "$$lib $$(pkg-config --modversion $$lib 2>/dev/null)"; \
		else \
			bad "$$lib development files missing"; fail=1; \
		fi; \
	done; \
	\
	echo; echo "terminal"; \
	case "$$COLORTERM" in \
		truecolor|24bit) ok "24-bit colour";; \
		*) note "COLORTERM is '$$COLORTERM' — the panel needs 24-bit colour to look right"; warn=1;; \
	esac; \
	if command -v fc-list >/dev/null 2>&1; then \
		if fc-list 2>/dev/null | grep -qi nerd; then ok "a nerd font is installed"; \
		else note "no Nerd Font found — block and media glyphs may not render"; warn=1; fi; \
	else \
		note "fontconfig absent, cannot check for a Nerd Font"; warn=1; \
	fi; \
	\
	echo; echo "radio (optional — the other two sources work without it)"; \
	if lsusb 2>/dev/null | grep -qiE 'rtl2832|rtl2838|realtek.*dvb'; then \
		ok "an RTL-SDR is plugged in"; \
		if lsmod 2>/dev/null | grep -q dvb_usb_rtl28xxu; then \
			note "DVB-T driver has claimed it: sudo modprobe -r dvb_usb_rtl28xxu dvb_usb_v2 rtl2832"; warn=1; \
		else \
			ok "DVB-T driver is not holding the device"; \
		fi; \
	else \
		note "no RTL-SDR detected — the tuner bay will say so and the rest still works"; \
	fi; \
	\
	echo; \
	if [ $$fail -ne 0 ]; then \
		echo "missing build dependencies. On this system, try:"; echo; \
		if   command -v pacman  >/dev/null 2>&1; then echo "    sudo pacman -S --needed base-devel pkgconf alsa-lib rtl-sdr"; \
		elif command -v apt-get >/dev/null 2>&1; then echo "    sudo apt install build-essential pkg-config libasound2-dev librtlsdr-dev"; \
		elif command -v dnf     >/dev/null 2>&1; then echo "    sudo dnf install gcc pkgconf-pkg-config alsa-lib-devel rtl-sdr-devel"; \
		elif command -v zypper  >/dev/null 2>&1; then echo "    sudo zypper install gcc pkg-config alsa-devel rtl-sdr-devel"; \
		elif command -v brew    >/dev/null 2>&1; then echo "    brew install pkg-config librtlsdr   # CoreAudio replaces ALSA on macOS"; \
		else echo "    install: a C toolchain, pkg-config, ALSA headers, librtlsdr headers"; fi; \
		echo; exit 1; \
	fi; \
	if [ $$warn -ne 0 ]; then \
		echo "buildable, with the caveats above."; \
	else \
		echo "all dependencies satisfied."; \
	fi

# ---------------------------------------------------------------------------
# Build and run
# ---------------------------------------------------------------------------

build: ## Debug build
	cargo build

release: ## Optimised build
	cargo build --release

run: release ## Build and run the panel
	./$(BIN)

install: release ## Install to $XDG_DATA_HOME/bin (default ~/.local/bin)
	@mkdir -p "$(BINDIR)"
	@install -m 755 "$(BIN)" "$(BINDIR)/ten-qd"
	@echo "installed $(BINDIR)/ten-qd"
	@case ":$$PATH:" in \
		*":$(BINDIR):"*) echo "$(BINDIR) is on your PATH — run 'ten-qd'";; \
		*) echo; echo "  $(BINDIR) is NOT on your PATH. Add it:"; \
		   echo "    export PATH=\"$(BINDIR):\$$PATH\"";; \
	esac

uninstall: ## Remove the installed binary (leaves the memory alone)
	@rm -f "$(BINDIR)/ten-qd" && echo "removed $(BINDIR)/ten-qd"
	@if [ -e "$(STATEDIR)/memory.toml" ]; then \
		echo "settings kept at $(STATEDIR)/memory.toml — 'make forget' clears them"; \
	fi

forget: ## Clear the 12-volt memory (presets, tone, last disc)
	@rm -rf "$(STATEDIR)" && echo "cleared $(STATEDIR)"

screenshot: release ## Render one frame to stdout and exit
	./$(BIN) --screenshot

radio-check: release ## Sweep the FM band and report signal per channel
	./$(BIN) --radio-check

# ---------------------------------------------------------------------------
# Quality gates
# ---------------------------------------------------------------------------

test: ## Run the test suite
	cargo test

lint: ## Run clippy, warnings as errors
	cargo clippy --all-targets -- -D warnings

fmt: ## Format the source
	cargo fmt

fmt-check: ## Check formatting without changing anything
	cargo fmt --check

check: lint test ## Run all quality gates

clean: ## Remove build artifacts
	cargo clean

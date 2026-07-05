# Toolchain lives outside the default shell PATH (brew rustup + cargo-installed cargo-xwin).
# Full path to cargo because Apple's make 3.81 ignores exported PATH when exec'ing recipes.
CARGO := /opt/homebrew/opt/rustup/bin/cargo
export PATH := $(HOME)/.cargo/bin:/opt/homebrew/opt/rustup/bin:$(PATH)

TARGET := x86_64-pc-windows-msvc

build:
	$(CARGO) xwin build --release --target $(TARGET)
	@ls -lh target/$(TARGET)/release/win-app-switcher.exe

# Console subsystem: double-clicking the exe opens a console that logs
# every key event the hook sees and how it was interpreted.
debug:
	$(CARGO) xwin build --target $(TARGET)
	@ls -lh target/$(TARGET)/debug/win-app-switcher.exe

test:
	$(CARGO) test

.PHONY: build debug test

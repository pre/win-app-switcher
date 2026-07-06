# Toolchain lives outside the default shell PATH (brew rustup + cargo-installed cargo-xwin).
# Full path to cargo because Apple's make 3.81 ignores exported PATH when exec'ing recipes.
CARGO := /opt/homebrew/opt/rustup/bin/cargo
export PATH := $(HOME)/.cargo/bin:/opt/homebrew/opt/rustup/bin:$(PATH)

TARGET := x86_64-pc-windows-msvc

build:
	$(CARGO) xwin build --release --target $(TARGET)
	@ls -lh target/$(TARGET)/release/win-app-switcher.exe

# The debug build allocates a console at startup (after the single-instance
# gate) that logs the build hash and every key event the hook sees.
debug:
	$(CARGO) xwin build --target $(TARGET)
	@ls -lh target/$(TARGET)/debug/win-app-switcher.exe

test:
	$(CARGO) test

# The pinned release-build image, shared with .github/workflows/release.yml:
# rebuilding a tagged commit with `make docker-build` reproduces the released
# exe so its sha256 can be verified independently. Bump together with the
# host toolchain and XWIN_VERSION in bin/build.
RUST_IMAGE ?= rust:1.96.1-bookworm

# Release build / tests inside the pinned image. Named volumes cache crates
# and the xwin-downloaded Windows SDK between runs; CI overrides these with
# absolute host paths (bind mounts) so actions/cache can persist them.
CARGO_CACHE ?= win-app-switcher-cargo
XWIN_CACHE ?= win-app-switcher-xwin

docker-build docker-test:
	docker run --rm -v "$(CURDIR)":/src -w /src \
		-v "$(CARGO_CACHE)":/usr/local/cargo/registry \
		-v "$(XWIN_CACHE)":/root/.cache/cargo-xwin \
		$(RUST_IMAGE) bin/build $(@:docker-%=%)

.PHONY: build debug test docker-build docker-test

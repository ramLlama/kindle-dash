#
# configurables
#
TARGET ?= arm-unknown-linux-musleabi

#
# exported envvars
#
# Stop macOS tar from writing AppleDouble (._*) sidecar files into the tarball.
export COPYFILE_DISABLE := 1
# The cross-rs images only publish amd64 manifests; tell Docker explicitly so it
# doesn't warn about a host/image platform mismatch on Apple Silicon.
export DOCKER_DEFAULT_PLATFORM := linux/amd64

#
# globals
#
GIT_SHA := $(shell git rev-parse --short HEAD)
GIT_DIRTY := $(if $(shell git status --porcelain),-dirty,)
# Single source of truth for the version: the crate manifest.
CARGO_VERSION := $(shell grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
VERSION := v${CARGO_VERSION}-${GIT_SHA}${GIT_DIRTY}
BIN := target/${TARGET}/release/kindle-dash
RUST_SRC := $(shell find src -name '*.rs')

# The tarball unpacks into a single top-level `kindle-dash/` folder holding two
# independently-installable trees (plus the README):
#   kindle-dash/kindle-dash/  -> copy to /mnt/us/kindle-dash   (the app: binary, assets, config)
#   kindle-dash/KUAL/         -> copy to /mnt/us/extensions    (the KUAL launcher entry)
dist: ${BIN}
	rm -rf dist
	@mkdir -p dist/kindle-dash/kindle-dash dist/kindle-dash/KUAL
	cp README.md dist/kindle-dash/
	cp ${BIN} dist/kindle-dash/kindle-dash/kindle-dash
	cp -r assets dist/kindle-dash/kindle-dash/
	cp config.toml.example dist/kindle-dash/kindle-dash/
	cp -r KUAL/kindle-dash dist/kindle-dash/KUAL/kindle-dash

${BIN}: ${RUST_SRC} Cargo.toml Cargo.lock
	cross build --release --target ${TARGET}

tarball: dist
	tar -C dist --zstd -cvf kindle-dash-${VERSION}.tar.zst ./

test:
	cargo test

lint:
	cargo clippy --all-targets

format:
	cargo fmt

clean:
	rm -rf dist target kindle-dash-*.tar.zst

.PHONY: dist tarball test lint format clean

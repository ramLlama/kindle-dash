#
# configurables
#
XH_VERSION ?= v0.26.1

#
# exported envvars
#
export CARGO_PROFILE_RELEASE_STRIP := true
# The cross-rs images only publish amd64 manifests; tell Docker explicitly so it
# doesn't warn about a host/image platform mismatch on Apple Silicon.
export DOCKER_DEFAULT_PLATFORM := linux/amd64

#
# globals
#
VERSION := v1.0.0-beta.4
SRC_FILES := $(shell find src -name '*.sh' -o -name '*.png')
NEXT_WAKEUP_SRC_FILES := $(shell find src/next-wakeup/src -name '*.rs')
TARGET_FILES := $(SRC_FILES:src/%=dist/%)

dist: dist/next-wakeup dist/xh dist/local/state ${TARGET_FILES}

tarball: dist
	tar -C dist -cvzf kindle-dash-${VERSION}.tgz ./

dist/%: src/%
	@echo "Copying $<"
	@mkdir -p $(@D)
	@cp "$<" "$@"

dist/next-wakeup: ${NEXT_WAKEUP_SRC_FILES}
	@mkdir -p dist
	cd src/next-wakeup && cross build --release --target arm-unknown-linux-musleabi
	cp src/next-wakeup/target/arm-unknown-linux-musleabi/release/next-wakeup dist/

dist/xh: tmp/xh
	@mkdir -p dist
	cd tmp/xh && cross build --release --target arm-unknown-linux-musleabi
	cp tmp/xh/target/arm-unknown-linux-musleabi/release/xh dist/

tmp/xh:
	mkdir -p tmp/
	git clone --depth 1 --branch ${XH_VERSION} https://github.com/ducaale/xh.git tmp/xh

dist/local/state:
	mkdir -p dist/local/state

clean:
	rm -rf dist/* src/next-wakeup/target tmp

watch:
	watchexec -w src/ -p -- make

format:
	shfmt -i 2 -w -l src/**/*.sh

.PHONY: clean watch tarball format

# Everyday entry points. All builds are release: Bevy is unusable in debug.
#
#   make run                 the viewer against the running game
#   make run CLOUDS=cumulus=0.6 VIEW=0,-30   with a forced sky and camera aim
#   make lab                 the tree generator bench (no game needed)
#   make shot                a screenshot of the viewer, saved to shots/
#   make budget              where the triangles go over the live window
#   make test                unit tests (no game needed)
#   make check               format check, clippy, tests

CARGO ?= cargo
BIN    = target/release/dwarf-eye
LAB    = target/release/tree-lab
SHOTS  = shots

# Pass-through knobs; empty means the app's own default.
CLOUDS ?=
VIEW   ?=
CAM    ?=
TEXELS ?=
DELAY  ?= 14

env = $(if $(CLOUDS),DWARF_EYE_CLOUDS=$(CLOUDS)) \
      $(if $(VIEW),DWARF_EYE_VIEW=$(VIEW)) \
      $(if $(CAM),DWARF_EYE_CAM=$(CAM)) \
      $(if $(TEXELS),DWARF_EYE_TEXELS=$(TEXELS)) \
      RUST_LOG=warn,dwarf_eye=info

.PHONY: all build run lab shot lab-shot budget horizon test check fmt clippy clean clean-cache

all: build

build:
	$(CARGO) build --release --workspace

run: build
	$(env) ./$(BIN)

lab: build
	$(env) ./$(LAB)

$(SHOTS):
	mkdir -p $(SHOTS)

shot: build | $(SHOTS)
	$(env) DWARF_EYE_SHOT=$(SHOTS)/viewer-$$(date +%H%M%S).png:$(DELAY) ./$(BIN)
	@ls -t $(SHOTS)/viewer-*.png | head -1

lab-shot: build | $(SHOTS)
	$(env) DWARF_EYE_SHOT=$(SHOTS)/lab-$$(date +%H%M%S).png:6 ./$(LAB)
	@ls -t $(SHOTS)/lab-*.png | head -1

budget:
	$(CARGO) run --release -q -p dwarf-eye-world --example budget

horizon:
	$(CARGO) run --release -q -p dwarf-eye-world --example horizon

test:
	$(CARGO) test --release --workspace --lib

fmt:
	$(CARGO) fmt --all -- --check

clippy:
	$(CARGO) clippy --release --workspace --all-targets

check: fmt clippy test

clean:
	$(CARGO) clean

# The on-disk chunk cache for every world. The game rebuilds it as you play.
clean-cache:
	rm -rf $${XDG_CACHE_HOME:-$$HOME/.cache}/dwarf-eye/*-*/

# Everyday entry points. All builds are release: Bevy is unusable in debug.
# `make` alone prints the targets.

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

.PHONY: help all build run lab shot lab-shot budget horizon test walk-test check fmt clippy clean clean-cache

help:
	@echo "dwarf-eye"
	@echo
	@echo "  make build        build every crate (release)"
	@echo "  make run          the viewer, against the running game"
	@echo "  make lab          the tree generator bench (no game needed)"
	@echo "  make shot         screenshot of the viewer into shots/ after DELAY seconds"
	@echo "  make lab-shot     screenshot of the bench into shots/"
	@echo "  make budget       where the triangles go over the live window"
	@echo "  make horizon      what DFHack reports beyond the live window"
	@echo "  make test         unit tests (no game needed)"
	@echo "  make walk-test    walk sync against the running game; MOVES the adventurer"
	@echo "  make check        format check, clippy, tests"
	@echo "  make clean        remove build output"
	@echo "  make clean-cache  remove the on-disk chunk cache for every world"
	@echo
	@echo "Knobs, passed through to the app:"
	@echo "  CLOUDS=cumulus=0.6,cirrus=0.3   force a sky"
	@echo "  VIEW=yaw,pitch                  aim the camera, degrees"
	@echo "  CAM=2                           how far back the camera starts"
	@echo "  TEXELS=32                       texture density per tile"
	@echo "  DELAY=14                        seconds before a shot"
	@echo
	@echo "  e.g.  make run CLOUDS=cumulus=0.6 VIEW=0,-30"

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

# Walks the adventurer east and back through walk sync, and checks the game
# agrees. Needs a live game with an adventurer on open ground, clear to the
# east, so it is never part of `make test`.
walk-test:
	DWARF_EYE_LIVE=1 $(CARGO) test --release -p dwarf-eye --test walk_sync -- --nocapture

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

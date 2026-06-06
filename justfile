# flip-link task runner. Run `just` (or `just --list`) to see recipes.
#
# Host crates build with stable Rust from the repo root. The firmware is a
# separate package under firmware/ that builds for the thumbv7em embedded target
# (its .cargo/config.toml pins the target), so its recipes `cd firmware` first.

# Path to the built firmware artifact.
fap := "firmware/target/thumbv7em-none-eabihf/release/flip_link.fap"
# Remote path the FAP is uploaded to with `storage send`.
fap_remote := "/ext/apps/Tools/flip_link.fap"
# Daemon socket (matches flip-daemon's default).
sock := env_var_or_default("XDG_RUNTIME_DIR", "/tmp") / "flip-link.sock"

# List available recipes.
default:
    @just --list

# --- Host -----------------------------------------------------------------

# Build all host crates (flip, flip-daemon).
build:
    cargo build

# Run the hardware-free test suite (proto + core).
test:
    cargo test

# Format + lint check (CI-style; fails on any diff or warning).
check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings

# Apply rustfmt to host + firmware.
fmt:
    cargo fmt --all
    cd firmware && cargo fmt

# --- Firmware (FAP) -------------------------------------------------------

# Build the on-device firmware (.fap).
fw-build:
    cd firmware && cargo build --release

# Build, upload, and LAUNCH the FAP on a connected Flipper.
# This switches the device to dual-CDC and re-enumerates once (expected).
# Needs flipperzero-tools: `cargo install --locked flipperzero-tools`.
fw-run: fw-build
    run-fap {{fap}}

# Build + upload the FAP WITHOUT launching (fallback if run-fap misbehaves).
# Then launch it from the Flipper's Apps > Tools menu.
fw-send: fw-build
    storage send {{fap}} {{fap_remote}}

# Build both host and firmware.
build-all: build fw-build

# --- End-to-end (require the FAP running on-device) -----------------------

# Run `flip status` (auto-spawns the daemon). The FAP must already be running.
status: build
    ./target/debug/flip status

# Hardware go/no-go: PING/PONG round-trip over USB. FAP must be running.
hw-test:
    FLIPPER_HW=1 cargo test -p flip-core --test hw_ping -- --nocapture

# Full bench flow: flash + launch the FAP, then check status. Flashing the FAP
# re-enumerates USB onto a new port, and the daemon has no reconnect yet, so we
# restart it and pause for re-enumeration before `status`.
bench: fw-run
    -just daemon-stop
    sleep 2
    just status

# Reflash the FAP and clear the stale daemon (which is pinned to the old port).
# After this, launch the app on-device, then `just status`.
reflash: daemon-stop fw-send

# --- Daemon ---------------------------------------------------------------

# Run the daemon in the foreground (owns the device link). Ctrl-C to stop.
daemon: build
    ./target/debug/flip-daemon run

# Stop any running daemon and clear its socket.
daemon-stop:
    -pkill -f 'flip-daemon run'
    -rm -f "{{sock}}"

# --- Maintenance ----------------------------------------------------------

# Remove all build artifacts (host + firmware).
clean:
    cargo clean
    cd firmware && cargo clean

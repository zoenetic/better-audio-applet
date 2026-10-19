name := 'better-audio-applet'
appid := 'io.github.zoenetic.BetterAudioApplet'

rootdir := ''
prefix := '/usr'
debug := '0'
vendor := '0'

target := if debug == '1' { 'debug' } else { 'release' }
vendor_args := if vendor == '1' { '--frozen --offline' } else { '' }
debug_args := if debug == '1' { '' } else { '--release' }
cargo_args := vendor_args + ' ' + debug_args
targetdir := env('CARGO_TARGET_DIR', 'target')

sharedir := rootdir + prefix + '/share'
bindir := rootdir + prefix + '/bin'

default: build-release

# Compiles with debug profile
build-debug *args:
    cargo build {{ args }}

# Compiles with release profile
build-release *args: (build-debug '--release' args)

# Compile with a vendored tarball
build-vendored *args: vendor-extract (build-release '--frozen --offline' args)

# Runs clippy and the tests
check *args:
    cargo clippy --all-targets {{ args }} -- -D warnings
    cargo test {{ args }}

# Runs the applet outside the panel, for development
run *args:
    env RUST_LOG=better_audio_applet=debug RUST_BACKTRACE=full cargo run {{ args }}

# Installs files into the system
install: check-installable
    install -Dm0755 {{ targetdir }}/{{ target }}/{{ name }} {{ bindir }}/{{ name }}
    install -Dm0644 data/{{ appid }}.desktop {{ sharedir }}/applications/{{ appid }}.desktop

# Removes installed files
uninstall:
    rm -f {{ bindir }}/{{ name }}
    rm -f {{ sharedir }}/applications/{{ appid }}.desktop

# Vendor Cargo dependencies locally
vendor:
    mkdir -p .cargo
    cargo vendor --locked | head -n -1 > .cargo/config.toml
    echo 'directory = "vendor"' >> .cargo/config.toml
    tar pcf vendor.tar vendor
    rm -rf vendor

# Refuses a missing binary, or one built in a Nix shell (it loads libraries
# from the Nix store, so the panel can't start it)
[private]
check-installable:
    #!/usr/bin/env sh
    set -eu
    bin='{{ targetdir }}/{{ target }}/{{ name }}'
    if [ ! -x "$bin" ]; then
        echo "error: $bin not found, run 'just' first" >&2
        exit 1
    fi
    if readelf -l "$bin" 2>/dev/null | grep -q 'interpreter: /nix/store/'; then
        echo "error: $bin was built in a Nix shell and only runs there." >&2
        echo "Rebuild it outside Nix with 'just', then install again." >&2
        exit 1
    fi

# Extracts vendored dependencies
[private]
vendor-extract:
    rm -rf vendor
    tar pxf vendor.tar

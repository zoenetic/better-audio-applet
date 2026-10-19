name := 'better-audio-applet'
appid := 'io.github.zoenetic.BetterAudioApplet'
prefix := '/usr'

default: build

build:
    cargo build --release

# System-wide install (run with sudo after `just build`)
install:
    install -Dm0755 target/release/{{name}} {{prefix}}/bin/{{name}}
    install -Dm0644 data/{{appid}}.desktop {{prefix}}/share/applications/{{appid}}.desktop

uninstall:
    rm -f {{prefix}}/bin/{{name}} {{prefix}}/share/applications/{{appid}}.desktop

# Install for the current user only, no sudo needed
install-local: build
    install -Dm0755 target/release/{{name}} ~/.local/bin/{{name}}
    mkdir -p ~/.local/share/applications
    sed "s|^Exec=.*|Exec=$HOME/.local/bin/{{name}}|" data/{{appid}}.desktop \
        > ~/.local/share/applications/{{appid}}.desktop

uninstall-local:
    rm -f ~/.local/bin/{{name}} ~/.local/share/applications/{{appid}}.desktop

# Run from the terminal with logs (the panel normally starts it)
run:
    RUST_LOG=better_audio_applet=debug cargo run --release

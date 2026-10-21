# Better Audio applet for COSMIC

A panel applet that replaces COSMIC's built-in sound applet. On top of what the
stock one does, it lets you set the **volume and output device of each app** from
the panel, without opening COSMIC Settings.

- Output and Input: tap the heading to pick the device; volume slider and mute.
- Every app making sound gets one line in a card: its icon (click to mute), its
  name and a volume slider. Apps with several streams (like browser tabs) share
  one line with a stream count.
- Tap an app's name to pick its device. **Default** means it follows the Output
  device; picking a device pins the app to it, and a small tag shows where it is
  pinned. WirePlumber remembers the pin next time the app starts; picking Default
  again clears it. The same panel has a slider for each of the app's streams.
- Apps using the mic are folded into one line; tap it to adjust them.
- The app list scrolls once it gets long, so the popup stays a sensible size.
- Now playing bar with previous / play-pause / next (click it to open the player).
- On the panel icon: scroll to change volume, middle-click to mute.

## Requirements

- COSMIC desktop
- PipeWire with `pipewire-pulse` and WirePlumber (the default on Fedora)

Build dependencies on Fedora:

```sh
sudo dnf install cargo just clang libxkbcommon-devel wayland-devel \
    pulseaudio-libs-devel pipewire-devel
```

## Build and install

```sh
just
sudo just install
```

Then open *Settings → Desktop → Panel → Configure panel applets*, add
**Better Audio**, and remove the stock **Sound** applet.

To remove: `sudo just uninstall`.

### From a release

Each [release](https://github.com/zoenetic/better-audio-applet/releases) has a
prebuilt x86_64 tarball, so no build dependencies are needed. Unpack it, then
from inside the folder:

```sh
sudo just install
```

## Development

```sh
just check    # clippy + tests
just run      # run outside the panel, with debug logs
just vendor   # bundle crates into vendor.tar for offline builds (just build-vendored)
```

Code layout:

- `src/app.rs` – the applet (panel button, popup)
- `src/model.rs` – known devices and app streams, grouping, pinned devices
- `src/backend.rs` – starts the audio threads, one handle for the UI
- `src/pulse.rs` – volumes, mutes and defaults through `pipewire-pulse`
- `src/pw.rs` – per-app device pins through PipeWire metadata

## How it works

- Volumes, mutes and default devices go through PulseAudio's API, which
  PipeWire provides (`pipewire-pulse`).
- Per-app device choice is stored in PipeWire's `default` metadata
  (`target.object`), the same way COSMIC Settings does it, so WirePlumber
  remembers it.

Licensed GPL-3.0. `src/mpris_subscription.rs` and `src/mouse_area.rs` come from
[pop-os/cosmic-applets](https://github.com/pop-os/cosmic-applets).

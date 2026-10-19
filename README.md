# Better Audio applet for COSMIC

A panel applet that replaces COSMIC's built-in sound applet. On top of what the
stock one does, it lets you set the **volume and output device of each app** from
the panel, without opening COSMIC Settings.

- Output and Input: pick the device, set the volume, mute.
- **Playing**: every app making sound gets a mute button, a volume slider and a
  device dropdown. Apps with several streams (like browser tabs) share one row;
  click "N streams" to adjust each one.
- **Using mic**: the same for apps that are recording.
- **Default** in an app's dropdown means it follows the Output device. Picking a
  device pins the app to it. WirePlumber remembers the pin next time the app
  starts; picking Default again clears it.
- The app lists scroll once they get long, so the popup stays a sensible size.
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

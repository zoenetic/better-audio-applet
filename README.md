# Better Audio applet for COSMIC

A panel applet that replaces COSMIC's built-in sound applet. On top of what the
stock one does, it lets you set the **volume and output device of each app** from
the panel, without opening COSMIC Settings.

- The output device's name is the heading, with the main volume under it, and a
  compact mic volume line. Tap the device name (or the mic's name) to see every
  device with its own volume and pick the default.
- Every app making sound is listed up front: a large icon (click to mute), its
  name, a wide volume slider, and where it plays ("Default ›", or the device it
  is pinned to).
- Tap where an app plays to open its page: pick its device (**Default** follows
  the Output device) and adjust each of its streams, like separate browser tabs.
  WirePlumber remembers the pin next time the app starts; picking Default again
  clears it.
- Apps using the mic are one line that opens their own page.
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

After installing a new build, restart the applet so the panel picks it up
(`pkill better-audio-ap`; the panel starts it again). Check which build is
installed with `better-audio-applet --version`, which shows the git commit.

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
just watch-streams   # print every sound stream as it appears, and whether it's listed
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

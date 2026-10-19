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

## Build and install (Fedora)

```sh
sudo dnf install cargo rust just clang pulseaudio-libs-devel pipewire-devel \
    libxkbcommon-devel wayland-devel expat-devel fontconfig-devel freetype-devel \
    libinput-devel systemd-devel
just build
sudo just install        # or: just install-local (no sudo, installs to ~/.local)
```

Then open **COSMIC Settings → Desktop → Panel → Applets**, add **Better Audio**,
and remove the stock **Sound** applet.

To see logs, remove the applet from the panel and run `just run` in a terminal.

## How it works

- Volumes, mutes and default devices go through PulseAudio's API, which
  PipeWire provides (`pipewire-pulse`).
- Per-app device choice is stored in PipeWire's `default` metadata
  (`target.object`), the same way COSMIC Settings does it, so WirePlumber
  remembers it.

Licensed GPL-3.0. `src/mpris_subscription.rs` and `src/mouse_area.rs` come from
[pop-os/cosmic-applets](https://github.com/pop-os/cosmic-applets).

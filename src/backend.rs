// SPDX-License-Identifier: GPL-3.0-only

//! Starts the PulseAudio and PipeWire threads and gives the UI one handle to both.

use futures::channel::mpsc::UnboundedSender;
use std::sync::{Arc, Mutex, mpsc};

use crate::pulse::{self, Device, Kind, Stream};
use crate::pw;

#[derive(Debug, Clone)]
pub enum Event {
    PulseConnected,
    PulseDisconnected,
    Defaults {
        sink: String,
        source: String,
    },
    Device(Device),
    DeviceRemoved(Kind, u32),
    Stream(Stream),
    StreamRemoved(Kind, u32),
    /// A stream node's `target.object` or `target.node` changed (`None` key: both);
    /// a `None` value removes it.
    Target(u32, Option<TargetKey>, Option<String>),
    /// The PipeWire connection was lost; all known targets are stale.
    TargetsReset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKey {
    /// `target.object`: a device's `object.serial` or node name.
    Object,
    /// `target.node`: a device's node id, written by older WirePlumber versions.
    Node,
}

#[derive(Clone)]
pub struct Handle {
    pulse: mpsc::Sender<pulse::Command>,
    pw: pw::SharedSender,
}

impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("backend::Handle")
    }
}

impl Handle {
    pub fn start(events: UnboundedSender<Event>) -> Self {
        let (pulse_tx, pulse_rx) = mpsc::channel();
        let pw: pw::SharedSender = Arc::new(Mutex::new(None));

        std::thread::spawn({
            let events = events.clone();
            move || pulse::run(pulse_rx, events)
        });
        std::thread::spawn({
            let pw = Arc::clone(&pw);
            move || pw::run(pw, events)
        });

        Self {
            pulse: pulse_tx,
            pw,
        }
    }

    pub fn send(&self, command: pulse::Command) {
        _ = self.pulse.send(command);
    }

    /// Pin a stream to a device, or with `None` let it follow the default device again.
    pub fn set_target(&self, stream: &Stream, device: Option<&Device>, default: Option<&Device>) {
        if let Some(subject) = stream.object_id
            && let Some(tx) = self.pw.lock().unwrap().as_ref()
            && (device.is_none() || device.is_some_and(|d| d.serial.is_some()))
        {
            _ = tx.send(pw::Request::SetTarget {
                subject,
                serial: device.and_then(|d| d.serial.clone()),
            });
            return;
        }

        // No PipeWire metadata: move the stream directly. Without metadata there is
        // no way to un-pin, so "Default" moves it to the current default device.
        if let Some(device) = device.or(default) {
            self.send(pulse::Command::MoveStream(
                stream.kind,
                stream.index,
                device.index,
            ));
        }
    }
}

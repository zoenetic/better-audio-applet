// SPDX-License-Identifier: GPL-3.0-only

//! What the applet knows about devices and app streams.

use libpulse_binding::volume::{ChannelVolumes, Volume};
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use crate::backend::{Event, TargetKey};
use crate::pulse::{Device, Kind, Stream};

#[derive(Default)]
pub struct Model {
    pub connected: bool,
    sinks: BTreeMap<u32, Device>,
    sources: BTreeMap<u32, Device>,
    playback: BTreeMap<u32, Stream>,
    record: BTreeMap<u32, Stream>,
    default_sink: String,
    default_source: String,
    /// `target.object` and `target.node` metadata, keyed by stream node id.
    targets: HashMap<u32, Targets>,
    /// When the volume of a device or stream was last changed here.
    changed_here: HashMap<Volumed, Instant>,
}

/// Something with a volume: a device or a stream, by kind and index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Volumed {
    Device(Kind, u32),
    Stream(Kind, u32),
}

/// How long after a volume change here the server's reports of that volume are
/// ignored. While a slider is dragged, the server reports values that lag behind the
/// cursor (or are rounded to the hardware's steps); taking them would make the slider
/// jump back and forth under the cursor.
const HOLD: Duration = Duration::from_millis(500);

#[derive(Default)]
struct Targets {
    object: Option<String>,
    node: Option<String>,
}

/// All streams of one application.
pub struct Group<'a> {
    pub key: &'a str,
    pub name: &'a str,
    pub icon_name: Option<&'a str>,
    pub streams: Vec<&'a Stream>,
}

impl Group<'_> {
    pub fn percent(&self) -> u32 {
        self.streams
            .iter()
            .map(|s| percent(&s.volume))
            .max()
            .unwrap_or(0)
    }

    pub fn muted(&self) -> bool {
        self.streams.iter().all(|s| s.mute)
    }
}

impl Model {
    pub fn update(&mut self, event: Event) {
        match event {
            Event::PulseConnected => self.connected = true,
            Event::PulseDisconnected => {
                let targets = std::mem::take(&mut self.targets);
                *self = Self {
                    targets,
                    ..Self::default()
                };
            }
            Event::Defaults { sink, source } => {
                self.default_sink = sink;
                self.default_source = source;
            }
            Event::Device(mut device) => {
                let held = self.is_held(Volumed::Device(device.kind, device.index));
                let devices = self.devices_mut(device.kind);
                if held && let Some(known) = devices.get(&device.index) {
                    device.volume = known.volume;
                }
                devices.insert(device.index, device);
            }
            Event::DeviceRemoved(kind, index) => {
                self.devices_mut(kind).remove(&index);
            }
            Event::Stream(mut stream) => {
                let held = self.is_held(Volumed::Stream(stream.kind, stream.index));
                let streams = self.streams_mut(stream.kind);
                if held && let Some(known) = streams.get(&stream.index) {
                    stream.volume = known.volume;
                }
                streams.insert(stream.index, stream);
            }
            Event::StreamRemoved(kind, index) => {
                self.streams_mut(kind).remove(&index);
            }
            Event::Target(subject, key, value) => {
                let targets = self.targets.entry(subject).or_default();
                match key {
                    Some(TargetKey::Object) => targets.object = value,
                    Some(TargetKey::Node) => targets.node = value,
                    None => *targets = Targets::default(),
                }
                if targets.object.is_none() && targets.node.is_none() {
                    self.targets.remove(&subject);
                }
            }
            Event::TargetsReset => self.targets.clear(),
        }
    }

    /// Note that a volume was just changed here, so reports of it are held off.
    pub fn changed_here(&mut self, what: Volumed) {
        self.changed_here.insert(what, Instant::now());
    }

    fn is_held(&mut self, what: Volumed) -> bool {
        match self.changed_here.get(&what) {
            Some(at) if at.elapsed() < HOLD => true,
            Some(_) => {
                self.changed_here.remove(&what);
                false
            }
            None => false,
        }
    }

    pub fn devices_mut(&mut self, kind: Kind) -> &mut BTreeMap<u32, Device> {
        match kind {
            Kind::Output => &mut self.sinks,
            Kind::Input => &mut self.sources,
        }
    }

    pub fn streams_mut(&mut self, kind: Kind) -> &mut BTreeMap<u32, Stream> {
        match kind {
            Kind::Output => &mut self.playback,
            Kind::Input => &mut self.record,
        }
    }

    fn devices(&self, kind: Kind) -> &BTreeMap<u32, Device> {
        match kind {
            Kind::Output => &self.sinks,
            Kind::Input => &self.sources,
        }
    }

    /// Devices in the order shown in dropdowns.
    pub fn device_list(&self, kind: Kind) -> Vec<&Device> {
        let mut list: Vec<_> = self.devices(kind).values().collect();
        list.sort_by(|a, b| a.description.cmp(&b.description));
        list
    }

    pub fn default_device(&self, kind: Kind) -> Option<&Device> {
        let name = match kind {
            Kind::Output => &self.default_sink,
            Kind::Input => &self.default_source,
        };
        self.devices(kind).values().find(|d| d.name == *name)
    }

    pub fn default_device_mut(&mut self, kind: Kind) -> Option<&mut Device> {
        let name = match kind {
            Kind::Output => self.default_sink.clone(),
            Kind::Input => self.default_source.clone(),
        };
        self.devices_mut(kind).values_mut().find(|d| d.name == name)
    }

    /// The device a stream is pinned to, or `None` if it follows the default device.
    pub fn pinned_device(&self, stream: &Stream) -> Option<&Device> {
        let devices = self.devices(stream.kind);
        if let Some(targets) = stream.object_id.and_then(|id| self.targets.get(&id)) {
            let by_object = targets.object.as_deref().and_then(|t| {
                devices
                    .values()
                    .find(|d| d.serial.as_deref() == Some(t) || d.name == t)
            });
            let by_node = targets
                .node
                .as_deref()
                .and_then(|t| devices.values().find(|d| d.node_id.as_deref() == Some(t)));
            if let Some(device) = by_object.or(by_node) {
                return Some(device);
            }
        }

        // Not pinned through metadata, but playing somewhere other than the default:
        // show where it really is rather than pretending it follows the default.
        let current = devices.get(&stream.device)?;
        let default = self.default_device(stream.kind)?;
        (current.index != default.index).then_some(current)
    }

    /// Streams grouped by application, in the order the apps first appeared.
    pub fn groups(&self, kind: Kind) -> Vec<Group<'_>> {
        let streams = match kind {
            Kind::Output => &self.playback,
            Kind::Input => &self.record,
        };
        let mut groups: Vec<Group<'_>> = Vec::new();
        for stream in streams.values() {
            match groups.iter_mut().find(|g| g.key == stream.app_key) {
                Some(group) => {
                    if group.icon_name.is_none() {
                        group.icon_name = stream.icon_name.as_deref();
                    }
                    group.streams.push(stream);
                }
                None => groups.push(Group {
                    key: &stream.app_key,
                    name: &stream.app_name,
                    icon_name: stream.icon_name.as_deref(),
                    streams: vec![stream],
                }),
            }
        }
        // System sounds go last; they matter least.
        groups.sort_by_key(|g| g.key == "::events");
        groups
    }

    pub fn group_streams(&self, kind: Kind, key: &str) -> Vec<Stream> {
        self.groups(kind)
            .into_iter()
            .find(|g| g.key == key)
            .map(|g| g.streams.into_iter().cloned().collect())
            .unwrap_or_default()
    }
}

pub fn percent(volume: &ChannelVolumes) -> u32 {
    (volume.max().0 as f64 * 100.0 / Volume::NORMAL.0 as f64).round() as u32
}

/// Scale a volume so its loudest channel is at `percent`, keeping the balance.
pub fn with_percent(volume: &ChannelVolumes, percent: u32) -> ChannelVolumes {
    let mut volume = *volume;
    volume.scale(Volume(
        (percent as f64 / 100.0 * Volume::NORMAL.0 as f64).round() as u32,
    ));
    volume
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speakers(percent: u32) -> Device {
        let mut volume = ChannelVolumes::default();
        volume.set(2, Volume::NORMAL);
        Device {
            kind: Kind::Output,
            index: 1,
            name: "speakers".into(),
            description: "Speakers".into(),
            volume: with_percent(&volume, percent),
            mute: false,
            serial: None,
            node_id: None,
        }
    }

    fn shown(model: &Model) -> u32 {
        percent(&model.sinks[&1].volume)
    }

    #[test]
    fn server_reports_are_held_off_after_a_change_here() {
        let mut model = Model::default();
        model.update(Event::Device(speakers(40)));

        // The slider moved to 60 here; a lagging report of 50 must not move it back.
        model.devices_mut(Kind::Output).get_mut(&1).unwrap().volume =
            with_percent(&speakers(0).volume, 60);
        model.changed_here(Volumed::Device(Kind::Output, 1));
        model.update(Event::Device(speakers(50)));
        assert_eq!(shown(&model), 60);

        // Once the hold is over, changes made elsewhere show again.
        std::thread::sleep(HOLD + Duration::from_millis(50));
        model.update(Event::Device(speakers(30)));
        assert_eq!(shown(&model), 30);
    }
}

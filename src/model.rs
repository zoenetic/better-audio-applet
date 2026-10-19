// SPDX-License-Identifier: GPL-3.0-only

//! What the applet knows about devices and app streams.

use libpulse_binding::volume::{ChannelVolumes, Volume};
use std::collections::{BTreeMap, HashMap};

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
}

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
            Event::Device(device) => {
                self.devices_mut(device.kind).insert(device.index, device);
            }
            Event::DeviceRemoved(kind, index) => {
                self.devices_mut(kind).remove(&index);
            }
            Event::Stream(stream) => {
                self.streams_mut(stream.kind).insert(stream.index, stream);
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

    fn devices_mut(&mut self, kind: Kind) -> &mut BTreeMap<u32, Device> {
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

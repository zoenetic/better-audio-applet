// SPDX-License-Identifier: GPL-3.0-only

//! PulseAudio (pipewire-pulse) connection: devices, per-app streams, volumes and mutes.

use futures::channel::mpsc::UnboundedSender;
use libpulse_binding as pulse;
use pulse::callbacks::ListResult;
use pulse::context::introspect::{SinkInfo, SinkInputInfo, SourceInfo, SourceOutputInfo};
use pulse::context::subscribe::{Facility, InterestMaskSet, Operation};
use pulse::context::{Context, FlagSet, State};
use pulse::mainloop::threaded::Mainloop;
use pulse::proplist::{Proplist, properties};
use pulse::volume::ChannelVolumes;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use crate::backend::Event;

/// Whether a device or stream deals with output (playback) or input (recording).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Output,
    Input,
}

#[derive(Debug, Clone)]
pub struct Device {
    pub kind: Kind,
    pub index: u32,
    pub name: String,
    pub description: String,
    pub volume: ChannelVolumes,
    pub mute: bool,
    /// PipeWire `object.serial`, used as the value of `target.object` metadata.
    pub serial: Option<String>,
    /// PipeWire node id, which older WirePlumber versions store in `target.node`.
    pub node_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Stream {
    pub kind: Kind,
    pub index: u32,
    /// Key used to group streams of the same application together.
    pub app_key: String,
    pub app_name: String,
    pub icon_name: Option<String>,
    pub media_name: String,
    /// Index of the sink or source the stream is currently connected to.
    pub device: u32,
    pub volume: ChannelVolumes,
    pub mute: bool,
    /// PipeWire node id, the subject of the stream's `target.object` metadata.
    pub object_id: Option<u32>,
}

#[derive(Debug)]
pub enum Command {
    SetDeviceVolume(Kind, u32, ChannelVolumes),
    SetDeviceMute(Kind, u32, bool),
    SetDefault(Kind, String),
    SetStreamVolume(Kind, u32, ChannelVolumes),
    SetStreamMute(Kind, u32, bool),
    /// Fallback for moving a stream when PipeWire metadata is unavailable.
    MoveStream(Kind, u32, u32),
}

/// Streams from these apps are level meters, not something a person wants to control.
const IGNORED_APP_IDS: &[&str] = &[
    "org.PulseAudio.pavucontrol",
    "org.gnome.VolumeControl",
    "org.kde.kmixd",
    "org.kde.plasma-pa",
    "com.system76.CosmicSettings",
    "com.system76.CosmicSettingsDaemon",
];

/// COSMIC's own desktop processes. Their sounds (like the "pop" on every volume
/// change) last a moment; listing them made the popup grow and shrink while a
/// slider was dragged. COSMIC apps that play real audio, like COSMIC Player, are
/// not in this list.
const IGNORED_BINARIES: &[&str] = &[
    "cosmic-panel",
    "cosmic-osd",
    "cosmic-comp",
    "cosmic-settings-daemon",
    "cosmic-applets",
    "cosmic-notifications",
    "cosmic-greeter",
    "xdg-desktop-portal-cosmic",
];

pub fn run(commands: Receiver<Command>, events: UnboundedSender<Event>) {
    loop {
        if let Err(why) = session(&commands, &events) {
            tracing::warn!("pulse session ended: {why}");
        }
        _ = events.unbounded_send(Event::PulseDisconnected);
        // Drop commands issued while disconnected.
        while commands.try_recv().is_ok() {}
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn session(commands: &Receiver<Command>, events: &UnboundedSender<Event>) -> Result<(), String> {
    let mut proplist = Proplist::new().ok_or("proplist")?;
    _ = proplist.set_str(properties::APPLICATION_NAME, "Better Audio Applet");
    _ = proplist.set_str(properties::APPLICATION_ID, crate::APP_ID);

    let mainloop = Rc::new(RefCell::new(Mainloop::new().ok_or("mainloop")?));
    let context = Rc::new(RefCell::new(
        Context::new_with_proplist(&*mainloop.borrow(), "better-audio-applet", &proplist)
            .ok_or("context")?,
    ));

    {
        let ml = Rc::clone(&mainloop);
        context
            .borrow_mut()
            .set_state_callback(Some(Box::new(move || unsafe {
                (*ml.as_ptr()).signal(false);
            })));
    }

    context
        .borrow_mut()
        .connect(None, FlagSet::NOFAIL, None)
        .map_err(|e| format!("{e:?}"))?;

    mainloop.borrow_mut().lock();
    mainloop
        .borrow_mut()
        .start()
        .map_err(|e| format!("{e:?}"))?;

    loop {
        match context.borrow().get_state() {
            State::Ready => break,
            State::Failed | State::Terminated => {
                mainloop.borrow_mut().unlock();
                mainloop.borrow_mut().stop();
                return Err("connection failed".into());
            }
            _ => mainloop.borrow_mut().wait(),
        }
    }
    context.borrow_mut().set_state_callback(None);

    _ = events.unbounded_send(Event::PulseConnected);

    let introspect = context.borrow().introspect();
    query_all(&introspect, events);

    {
        let introspect = context.borrow().introspect();
        let events = events.clone();
        context
            .borrow_mut()
            .set_subscribe_callback(Some(Box::new(move |facility, op, index| {
                on_change(&introspect, &events, facility, op, index);
            })));
    }
    context.borrow_mut().subscribe(
        InterestMaskSet::SINK
            | InterestMaskSet::SOURCE
            | InterestMaskSet::SINK_INPUT
            | InterestMaskSet::SOURCE_OUTPUT
            | InterestMaskSet::SERVER,
        |_| {},
    );
    mainloop.borrow_mut().unlock();

    let result = loop {
        let command = match commands.recv_timeout(Duration::from_millis(500)) {
            Ok(command) => Some(command),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break Ok(()),
        };

        mainloop.borrow_mut().lock();
        if context.borrow().get_state() != State::Ready {
            mainloop.borrow_mut().unlock();
            break Err("connection lost".to_string());
        }
        if let Some(command) = command {
            execute(&mut context.borrow_mut(), command);
        }
        mainloop.borrow_mut().unlock();
    };

    mainloop.borrow_mut().lock();
    context.borrow_mut().set_subscribe_callback(None);
    context.borrow_mut().disconnect();
    mainloop.borrow_mut().unlock();
    mainloop.borrow_mut().stop();
    result
}

fn execute(context: &mut Context, command: Command) {
    let mut introspect = context.introspect();
    match command {
        Command::SetDeviceVolume(Kind::Output, index, volume) => {
            introspect.set_sink_volume_by_index(index, &volume, None);
        }
        Command::SetDeviceVolume(Kind::Input, index, volume) => {
            introspect.set_source_volume_by_index(index, &volume, None);
        }
        Command::SetDeviceMute(Kind::Output, index, mute) => {
            introspect.set_sink_mute_by_index(index, mute, None);
        }
        Command::SetDeviceMute(Kind::Input, index, mute) => {
            introspect.set_source_mute_by_index(index, mute, None);
        }
        Command::SetDefault(Kind::Output, name) => {
            context.set_default_sink(&name, |_| {});
        }
        Command::SetDefault(Kind::Input, name) => {
            context.set_default_source(&name, |_| {});
        }
        Command::SetStreamVolume(Kind::Output, index, volume) => {
            introspect.set_sink_input_volume(index, &volume, None);
        }
        Command::SetStreamVolume(Kind::Input, index, volume) => {
            introspect.set_source_output_volume(index, &volume, None);
        }
        Command::SetStreamMute(Kind::Output, index, mute) => {
            introspect.set_sink_input_mute(index, mute, None);
        }
        Command::SetStreamMute(Kind::Input, index, mute) => {
            introspect.set_source_output_mute(index, mute, None);
        }
        Command::MoveStream(Kind::Output, index, device) => {
            introspect.move_sink_input_by_index(index, device, None);
        }
        Command::MoveStream(Kind::Input, index, device) => {
            introspect.move_source_output_by_index(index, device, None);
        }
    }
}

fn query_all(
    introspect: &pulse::context::introspect::Introspector,
    events: &UnboundedSender<Event>,
) {
    query_server(introspect, events);

    let tx = events.clone();
    introspect.get_sink_info_list(move |r| {
        if let ListResult::Item(info) = r {
            _ = tx.unbounded_send(Event::Device(sink(info)));
        }
    });
    let tx = events.clone();
    introspect.get_source_info_list(move |r| {
        if let ListResult::Item(info) = r
            && let Some(device) = source(info)
        {
            _ = tx.unbounded_send(Event::Device(device));
        }
    });
    let tx = events.clone();
    introspect.get_sink_input_info_list(move |r| {
        if let ListResult::Item(info) = r
            && let Some(stream) = sink_input(info)
        {
            _ = tx.unbounded_send(Event::Stream(stream));
        }
    });
    let tx = events.clone();
    introspect.get_source_output_info_list(move |r| {
        if let ListResult::Item(info) = r
            && let Some(stream) = source_output(info)
        {
            _ = tx.unbounded_send(Event::Stream(stream));
        }
    });
}

fn query_server(
    introspect: &pulse::context::introspect::Introspector,
    events: &UnboundedSender<Event>,
) {
    let tx = events.clone();
    introspect.get_server_info(move |info| {
        _ = tx.unbounded_send(Event::Defaults {
            sink: info
                .default_sink_name
                .as_deref()
                .unwrap_or_default()
                .to_owned(),
            source: info
                .default_source_name
                .as_deref()
                .unwrap_or_default()
                .to_owned(),
        });
    });
}

fn on_change(
    introspect: &pulse::context::introspect::Introspector,
    events: &UnboundedSender<Event>,
    facility: Option<Facility>,
    op: Option<Operation>,
    index: u32,
) {
    let (Some(facility), Some(op)) = (facility, op) else {
        return;
    };

    if op == Operation::Removed {
        let event = match facility {
            Facility::Sink => Event::DeviceRemoved(Kind::Output, index),
            Facility::Source => Event::DeviceRemoved(Kind::Input, index),
            Facility::SinkInput => Event::StreamRemoved(Kind::Output, index),
            Facility::SourceOutput => Event::StreamRemoved(Kind::Input, index),
            _ => return,
        };
        _ = events.unbounded_send(event);
        return;
    }

    let tx = events.clone();
    match facility {
        Facility::Server => query_server(introspect, events),
        Facility::Sink => {
            introspect.get_sink_info_by_index(index, move |r| {
                if let ListResult::Item(info) = r {
                    _ = tx.unbounded_send(Event::Device(sink(info)));
                }
            });
        }
        Facility::Source => {
            introspect.get_source_info_by_index(index, move |r| {
                if let ListResult::Item(info) = r
                    && let Some(device) = source(info)
                {
                    _ = tx.unbounded_send(Event::Device(device));
                }
            });
        }
        Facility::SinkInput => {
            introspect.get_sink_input_info(index, move |r| {
                if let ListResult::Item(info) = r {
                    _ = tx.unbounded_send(match sink_input(info) {
                        Some(stream) => Event::Stream(stream),
                        None => Event::StreamRemoved(Kind::Output, index),
                    });
                }
            });
        }
        Facility::SourceOutput => {
            introspect.get_source_output_info(index, move |r| {
                if let ListResult::Item(info) = r {
                    _ = tx.unbounded_send(match source_output(info) {
                        Some(stream) => Event::Stream(stream),
                        None => Event::StreamRemoved(Kind::Input, index),
                    });
                }
            });
        }
        _ => {}
    }
}

fn sink(info: &SinkInfo) -> Device {
    Device {
        kind: Kind::Output,
        index: info.index,
        name: info.name.as_deref().unwrap_or_default().to_owned(),
        description: info.description.as_deref().unwrap_or_default().to_owned(),
        volume: info.volume,
        mute: info.mute,
        serial: info.proplist.get_str("object.serial"),
        node_id: info.proplist.get_str("object.id"),
    }
}

fn source(info: &SourceInfo) -> Option<Device> {
    // Monitors of outputs are not microphones.
    if info.monitor_of_sink.is_some() {
        return None;
    }
    Some(Device {
        kind: Kind::Input,
        index: info.index,
        name: info.name.as_deref().unwrap_or_default().to_owned(),
        description: info.description.as_deref().unwrap_or_default().to_owned(),
        volume: info.volume,
        mute: info.mute,
        serial: info.proplist.get_str("object.serial"),
        node_id: info.proplist.get_str("object.id"),
    })
}

fn sink_input(info: &SinkInputInfo) -> Option<Stream> {
    stream(
        Kind::Output,
        info.index,
        info.sink,
        info.name.as_deref(),
        &info.proplist,
        info.volume,
        info.mute,
    )
}

fn source_output(info: &SourceOutputInfo) -> Option<Stream> {
    stream(
        Kind::Input,
        info.index,
        info.source,
        info.name.as_deref(),
        &info.proplist,
        info.volume,
        info.mute,
    )
}

fn stream(
    kind: Kind,
    index: u32,
    device: u32,
    name: Option<&str>,
    props: &Proplist,
    volume: ChannelVolumes,
    mute: bool,
) -> Option<Stream> {
    let app_id = props.get_str(properties::APPLICATION_ID);
    if app_id
        .as_deref()
        .is_some_and(|id| IGNORED_APP_IDS.contains(&id))
    {
        return None;
    }
    // Streams that only exist to show a level meter.
    if props.get_str("stream.monitor").as_deref() == Some("true") {
        return None;
    }

    // Event sounds (notification blips, volume-change pops) last a moment, and
    // desktop sounds aren't something to give a slider to.
    if props.get_str(properties::MEDIA_ROLE).as_deref() == Some("event") {
        return None;
    }
    let binary = props.get_str(properties::APPLICATION_PROCESS_BINARY);
    if binary
        .as_deref()
        .is_some_and(|b| IGNORED_BINARIES.contains(&b) || b.starts_with("cosmic-applet"))
    {
        return None;
    }

    let media_name = props
        .get_str(properties::MEDIA_NAME)
        .or_else(|| name.map(str::to_owned))
        .unwrap_or_default();
    let app_name = props
        .get_str(properties::APPLICATION_NAME)
        .or_else(|| binary.clone())
        .unwrap_or_else(|| media_name.clone());
    let app_key = binary
        .clone()
        .unwrap_or_else(|| app_name.clone())
        .to_lowercase();

    let icon_name = props
        .get_str(properties::APPLICATION_ICON_NAME)
        .or(app_id)
        .or(binary);

    Some(Stream {
        kind,
        index,
        app_key,
        app_name,
        icon_name,
        media_name,
        device,
        volume,
        mute,
        object_id: props.get_str("object.id").and_then(|id| id.parse().ok()),
    })
}

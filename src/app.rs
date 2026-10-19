// SPDX-License-Identifier: GPL-3.0-only

use cosmic::iced::core::text::{Ellipsize, EllipsizeHeightLimit};
use cosmic::{
    Element, Task, app,
    applet::{
        menu_button, menu_control_padding, padded_control,
        token::subscription::{TokenRequest, TokenUpdate, activation_token_subscription},
    },
    cctk::sctk::reexports::calloop,
    cosmic_config::{Config, ConfigGet},
    iced::{
        self, Alignment, Length, Limits, Subscription,
        futures::{SinkExt, StreamExt},
        widget::{column, row},
        window,
    },
    surface, theme,
    widget::{Column, button, container, divider, dropdown, icon, scrollable, slider, space, text},
};
use mpris2_zbus::player::PlaybackStatus;
use std::collections::HashSet;

use crate::backend::{self, Handle};
use crate::model::{Group, Model, percent, with_percent};
use crate::mpris_subscription::{self, MprisRequest, MprisUpdate, PlayerStatus};
use crate::pulse::{Command, Kind};

/// Height at which the app lists start scrolling.
const PLAYBACK_LIST_HEIGHT: f32 = 300.0;
const RECORD_LIST_HEIGHT: f32 = 170.0;
const POPUP_WIDTH: f32 = 400.0;
const DEVICE_DROPDOWN_WIDTH: f32 = 150.0;
const WHEEL_STEP: i32 = 5;

#[derive(Default)]
pub struct Audio {
    core: cosmic::app::Core,
    popup: Option<window::Id>,
    backend: Option<Handle>,
    model: Model,
    /// Apps whose individual streams are shown.
    expanded: HashSet<(Kind, String)>,
    max_sink_volume: u32,
    max_source_volume: u32,
    player: Option<PlayerStatus>,
    token_tx: Option<calloop::channel::Sender<TokenRequest>>,
}

#[derive(Clone, Debug)]
pub enum Message {
    Backend(Handle),
    Audio(backend::Event),
    TogglePopup,
    CloseRequested(window::Id),
    Surface(surface::Action<Message>),
    SetDefault(Kind, usize),
    DeviceVolume(Kind, u32),
    ToggleDeviceMute(Kind),
    Wheel(i32),
    GroupVolume(Kind, String, u32),
    ToggleGroupMute(Kind, String),
    /// 0 is "Default"; anything else is a position in the device list, plus one.
    GroupDevice(Kind, String, usize),
    StreamVolume(Kind, u32, u32),
    ToggleStreamMute(Kind, u32),
    ToggleExpanded(Kind, String),
    Mpris(MprisUpdate),
    MprisRequest(MprisRequest),
    OpenSettings,
    Token(TokenUpdate),
}

impl cosmic::Application for Audio {
    type Message = Message;
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    const APP_ID: &'static str = crate::APP_ID;

    fn init(core: cosmic::app::Core, _flags: ()) -> (Self, app::Task<Message>) {
        let mut audio = Self {
            core,
            ..Default::default()
        };
        audio.read_amplification();
        (audio, Task::none())
    }

    fn core(&self) -> &cosmic::app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::app::Core {
        &mut self.core
    }

    fn style(&self) -> Option<iced::theme::Style> {
        Some(cosmic::applet::style())
    }

    fn update(&mut self, message: Message) -> app::Task<Message> {
        match message {
            Message::Backend(handle) => self.backend = Some(handle),
            Message::Audio(event) => self.model.update(event),

            Message::TogglePopup => {
                if let Some(id) = self.popup.take() {
                    return cosmic::surface::surface_task(cosmic::surface::action::destroy_popup(
                        id,
                    ));
                }
                self.read_amplification();
                return cosmic::surface::surface_task(cosmic::surface::action::app_popup(
                    |_| Default::default(),
                    |app: &mut Self| {
                        let id = window::Id::unique();
                        app.popup = Some(id);
                        app.core.applet.get_popup_settings(
                            app.core.main_window_id().unwrap(),
                            id,
                            None,
                            None,
                            None,
                        )
                    },
                    None,
                ));
            }
            Message::CloseRequested(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                }
            }
            Message::Surface(action) => {
                return cosmic::task::message(cosmic::Action::Surface(action));
            }

            Message::SetDefault(kind, pos) => {
                if let Some(device) = self.model.device_list(kind).get(pos) {
                    self.send(Command::SetDefault(kind, device.name.clone()));
                }
            }
            Message::DeviceVolume(kind, value) => self.set_device_volume(kind, value),
            Message::Wheel(0) => {}
            Message::Wheel(delta) => {
                let current = self
                    .model
                    .default_device(Kind::Output)
                    .map_or(0, |d| percent(&d.volume)) as i32;
                let value = (current + delta).clamp(0, self.max_sink_volume as i32);
                self.set_device_volume(Kind::Output, value as u32);
            }
            Message::ToggleDeviceMute(kind) => {
                if let Some(device) = self.model.default_device_mut(kind) {
                    device.mute = !device.mute;
                    let command = Command::SetDeviceMute(kind, device.index, device.mute);
                    self.send(command);
                }
            }

            Message::GroupVolume(kind, key, value) => {
                for stream in self.model.group_streams(kind, &key) {
                    self.set_stream_volume(kind, stream.index, value);
                }
            }
            Message::ToggleGroupMute(kind, key) => {
                let streams = self.model.group_streams(kind, &key);
                let mute = !streams.iter().all(|s| s.mute);
                for stream in streams {
                    self.set_stream_mute(kind, stream.index, mute);
                }
            }
            Message::GroupDevice(kind, key, choice) => {
                let devices = self.model.device_list(kind);
                let device = choice
                    .checked_sub(1)
                    .and_then(|pos| devices.get(pos).copied());
                let default = self.model.default_device(kind);
                if let Some(backend) = &self.backend {
                    for stream in self.model.group_streams(kind, &key) {
                        backend.set_target(&stream, device, default);
                    }
                }
            }
            Message::StreamVolume(kind, index, value) => self.set_stream_volume(kind, index, value),
            Message::ToggleStreamMute(kind, index) => {
                if let Some(stream) = self.model.streams_mut(kind).get(&index) {
                    let mute = !stream.mute;
                    self.set_stream_mute(kind, index, mute);
                }
            }
            Message::ToggleExpanded(kind, key) => {
                if !self.expanded.remove(&(kind, key.clone())) {
                    self.expanded.insert((kind, key));
                }
            }

            Message::Mpris(MprisUpdate::Player(player)) => self.player = Some(player),
            Message::Mpris(MprisUpdate::Finished | MprisUpdate::Setup) => self.player = None,
            Message::MprisRequest(request) => {
                let Some(player) = self.player.as_ref().map(|p| p.player.clone()) else {
                    return Task::none();
                };
                tokio::spawn(async move {
                    let result = match request {
                        MprisRequest::Play => player.play().await.map_err(|e| e.to_string()),
                        MprisRequest::Pause => player.pause().await.map_err(|e| e.to_string()),
                        MprisRequest::Next => player.next().await.map_err(|e| e.to_string()),
                        MprisRequest::Previous => {
                            player.previous().await.map_err(|e| e.to_string())
                        }
                        MprisRequest::Raise => match player.media_player().await {
                            Ok(media_player) => {
                                media_player.raise().await.map_err(|e| e.to_string())
                            }
                            Err(why) => Err(why.to_string()),
                        },
                    };
                    if let Err(why) = result {
                        tracing::error!("media player request failed: {why}");
                    }
                });
            }

            Message::OpenSettings => {
                if let Some(tx) = self.token_tx.as_ref() {
                    _ = tx.send(TokenRequest {
                        app_id: crate::APP_ID.to_string(),
                        exec: "cosmic-settings sound".to_string(),
                    });
                }
            }
            Message::Token(update) => match update {
                TokenUpdate::Init(tx) => self.token_tx = Some(tx),
                TokenUpdate::Finished => self.token_tx = None,
                TokenUpdate::ActivationToken { token, .. } => {
                    let mut cmd = std::process::Command::new("cosmic-settings");
                    cmd.arg("sound");
                    if let Some(token) = token {
                        cmd.env("XDG_ACTIVATION_TOKEN", &token);
                        cmd.env("DESKTOP_STARTUP_ID", &token);
                    }
                    tokio::spawn(cosmic::process::spawn(cmd));
                }
            },
        }

        Task::none()
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            Subscription::run(|| {
                iced::stream::channel(
                    64,
                    |mut output: futures::channel::mpsc::Sender<Message>| async move {
                        let (tx, mut rx) = futures::channel::mpsc::unbounded();
                        _ = output.send(Message::Backend(Handle::start(tx))).await;
                        while let Some(event) = rx.next().await {
                            _ = output.send(Message::Audio(event)).await;
                        }
                    },
                )
            }),
            mpris_subscription::mpris_subscription(0).map(Message::Mpris),
            activation_token_subscription(0).map(Message::Token),
        ])
    }

    fn view(&self) -> Element<'_, Message> {
        let button = self
            .core
            .applet
            .icon_button(self.output_icon_name())
            .on_press_down(Message::TogglePopup);

        let button = crate::mouse_area::MouseArea::new(button)
            .on_middle_press(Message::ToggleDeviceMute(Kind::Output))
            .on_mouse_wheel(|delta| {
                // A mouse wheel notch moves 5%; touchpads send many small pixel events.
                Message::Wheel(match delta {
                    iced::mouse::ScrollDelta::Lines { y, .. } => y.signum() as i32 * WHEEL_STEP,
                    iced::mouse::ScrollDelta::Pixels { y, .. } => y.signum() as i32,
                })
            });

        self.core.applet.autosize_window(button).into()
    }

    fn view_window(&self, _id: window::Id) -> Element<'_, Message> {
        let cosmic::cosmic_theme::Spacing {
            space_xxs, space_s, ..
        } = theme::active().cosmic().spacing;
        let separator =
            || padded_control(divider::horizontal::default()).padding([space_xxs, space_s]);

        let mut content = Column::new().padding([8, 0]).spacing(2);

        if !self.model.connected {
            content = content.push(padded_control(text::body(
                "Connecting to the sound server…",
            )));
        } else {
            content = content
                .push(self.device_section(Kind::Output))
                .push(self.device_section(Kind::Input))
                .push(separator());

            let playback = self.model.groups(Kind::Output);
            content = content.push(section_header(match playback.len() {
                0 => "No apps playing".to_string(),
                1 => "Playing · 1 app".to_string(),
                n => format!("Playing · {n} apps"),
            }));
            if !playback.is_empty() {
                content = content.push(self.app_list(Kind::Output, playback, PLAYBACK_LIST_HEIGHT));
            }

            let record = self.model.groups(Kind::Input);
            if !record.is_empty() {
                content = content
                    .push(separator())
                    .push(section_header(match record.len() {
                        1 => "Using mic · 1 app".to_string(),
                        n => format!("Using mic · {n} apps"),
                    }));
                content = content.push(self.app_list(Kind::Input, record, RECORD_LIST_HEIGHT));
            }
        }

        if let Some(player) = self.media_bar() {
            content = content.push(separator()).push(player);
        }

        content = content
            .push(separator())
            .push(menu_button(text::body("Sound settings…")).on_press(Message::OpenSettings));

        self.core
            .applet
            .popup_container(container(content))
            .limits(
                Limits::NONE
                    .min_height(1.0)
                    .min_width(POPUP_WIDTH)
                    .max_width(POPUP_WIDTH)
                    .max_height(1000.0),
            )
            .into()
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        Some(Message::CloseRequested(id))
    }
}

impl Audio {
    fn send(&self, command: Command) {
        if let Some(backend) = &self.backend {
            backend.send(command);
        }
    }

    fn read_amplification(&mut self) {
        let config = Config::new("com.system76.CosmicAudio", 1).ok();
        let get = |key: &str, default: bool| {
            config
                .as_ref()
                .and_then(|c| c.get::<bool>(key).ok())
                .unwrap_or(default)
        };
        self.max_sink_volume = if get("amplification_sink", true) {
            150
        } else {
            100
        };
        self.max_source_volume = if get("amplification_source", false) {
            150
        } else {
            100
        };
    }

    fn max_volume(&self, kind: Kind) -> u32 {
        match kind {
            Kind::Output => self.max_sink_volume,
            Kind::Input => self.max_source_volume,
        }
    }

    fn set_device_volume(&mut self, kind: Kind, value: u32) {
        if let Some(device) = self.model.default_device_mut(kind) {
            device.volume = with_percent(&device.volume, value);
            let command = Command::SetDeviceVolume(kind, device.index, device.volume);
            self.send(command);
        }
    }

    fn set_stream_volume(&mut self, kind: Kind, index: u32, value: u32) {
        if let Some(stream) = self.model.streams_mut(kind).get_mut(&index) {
            stream.volume = with_percent(&stream.volume, value);
            let command = Command::SetStreamVolume(kind, index, stream.volume);
            self.send(command);
        }
    }

    fn set_stream_mute(&mut self, kind: Kind, index: u32, mute: bool) {
        if let Some(stream) = self.model.streams_mut(kind).get_mut(&index) {
            stream.mute = mute;
            self.send(Command::SetStreamMute(kind, index, mute));
        }
    }

    fn output_icon_name(&self) -> &'static str {
        match self.model.default_device(Kind::Output) {
            Some(d) => volume_icon(Kind::Output, d.mute, percent(&d.volume)),
            None => "audio-volume-muted-symbolic",
        }
    }

    /// The Output or Input heading with its device picker and volume slider.
    fn device_section(&self, kind: Kind) -> Element<'_, Message> {
        let devices = self.model.device_list(kind);
        let default = self.model.default_device(kind);
        let selected = default.and_then(|d| devices.iter().position(|x| x.index == d.index));
        let names: Vec<String> = devices.iter().map(|d| d.description.clone()).collect();
        let (mute, value) = default.map_or((true, 0), |d| (d.mute, percent(&d.volume)));

        let picker: Element<'_, Message> = if names.is_empty() {
            text::caption("No devices").into()
        } else {
            self.dropdown(
                names,
                selected,
                move |pos| Message::SetDefault(kind, pos),
                220.0,
            )
        };

        let heading = row![
            text::heading(match kind {
                Kind::Output => "Output",
                Kind::Input => "Input",
            }),
            space::horizontal(),
            picker,
        ]
        .align_y(Alignment::Center)
        .spacing(8);

        let controls = row![
            mute_button(
                volume_icon(kind, mute, value),
                20,
                Message::ToggleDeviceMute(kind)
            ),
            slider(0..=self.max_volume(kind), value, move |v| {
                Message::DeviceVolume(kind, v)
            })
            .breakpoints(if self.max_volume(kind) > 100 {
                &[100]
            } else {
                &[]
            }),
            percent_label(mute, value),
        ]
        .align_y(Alignment::Center)
        .spacing(10);

        padded_control(column![heading, controls].spacing(8)).into()
    }

    fn app_list<'a>(
        &'a self,
        kind: Kind,
        groups: Vec<Group<'a>>,
        height: f32,
    ) -> Element<'a, Message> {
        let list = groups
            .into_iter()
            .fold(Column::new().spacing(4), |list, group| {
                list.push(self.app_row(kind, group))
            });

        container(scrollable(list).height(Length::Shrink))
            .max_height(height)
            .into()
    }

    fn app_row<'a>(&'a self, kind: Kind, group: Group<'a>) -> Element<'a, Message> {
        let key = group.key.to_string();
        let value = group.percent();
        let muted = group.muted();
        let max = self.max_volume(kind);

        let app_icon = icon::from_name(group.icon_name.unwrap_or("application-x-executable"))
            .size(24)
            .fallback(Some(icon::IconFallback::Names(vec![
                "application-x-executable".into(),
                "audio-x-generic".into(),
            ])))
            .icon();

        let mut heading = row![
            app_icon,
            text::body(group.name)
                .width(Length::Fill)
                .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1))),
        ]
        .align_y(Alignment::Center)
        .spacing(10);

        let expanded = self.expanded.contains(&(kind, key.clone()));
        if group.streams.len() > 1 {
            heading = heading.push(
                button::text(format!("{} streams", group.streams.len()))
                    .trailing_icon(icon::from_name(if expanded {
                        "go-up-symbolic"
                    } else {
                        "go-down-symbolic"
                    }))
                    .on_press(Message::ToggleExpanded(kind, key.clone())),
            );
        }

        // Every stream of the app is pinned to the same device, or the first one decides.
        let devices = self.model.device_list(kind);
        let pinned = group
            .streams
            .first()
            .and_then(|s| self.model.pinned_device(s));
        let selected = match pinned {
            Some(device) => devices
                .iter()
                .position(|d| d.index == device.index)
                .map(|p| p + 1),
            None => Some(0),
        };
        let names: Vec<String> = std::iter::once("Default".to_string())
            .chain(devices.iter().map(|d| d.description.clone()))
            .collect();
        heading = heading.push(self.dropdown(
            names,
            selected,
            {
                let key = key.clone();
                move |choice| Message::GroupDevice(kind, key.clone(), choice)
            },
            DEVICE_DROPDOWN_WIDTH,
        ));

        let controls = row![
            space::horizontal().width(Length::Fixed(28.0)),
            mute_button(
                volume_icon(kind, muted, value),
                16,
                Message::ToggleGroupMute(kind, key.clone()),
            ),
            slider(0..=max, value, {
                let key = key.clone();
                move |v| Message::GroupVolume(kind, key.clone(), v)
            }),
            percent_label(muted, value),
        ]
        .align_y(Alignment::Center)
        .spacing(10);

        let mut row = column![heading, controls].spacing(2);

        if expanded {
            for stream in &group.streams {
                let value = percent(&stream.volume);
                let index = stream.index;
                row = row.push(
                    row![
                        space::horizontal().width(Length::Fixed(28.0)),
                        mute_button(
                            volume_icon(kind, stream.mute, value),
                            16,
                            Message::ToggleStreamMute(kind, index),
                        ),
                        text::caption(stream.media_name.as_str())
                            .width(Length::Fixed(110.0))
                            .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1))),
                        slider(0..=max, value, move |v| Message::StreamVolume(
                            kind, index, v
                        )),
                        percent_label(stream.mute, value),
                    ]
                    .align_y(Alignment::Center)
                    .spacing(8),
                );
            }
        }

        padded_control(row).padding([4, 16]).into()
    }

    fn dropdown<'a>(
        &self,
        names: Vec<String>,
        selected: Option<usize>,
        on_select: impl Fn(usize) -> Message + Send + Sync + 'static,
        width: f32,
    ) -> Element<'a, Message> {
        let dropdown = dropdown(names, selected, on_select).width(Length::Fixed(width));
        match self.popup {
            Some(parent) => dropdown.with_popup(parent, Message::Surface, |m| m).into(),
            None => dropdown.into(),
        }
    }

    fn media_bar(&self) -> Option<Element<'_, Message>> {
        let player = self.player.as_ref()?;

        let mut bar = cosmic::widget::Row::new()
            .align_y(Alignment::Center)
            .spacing(10);
        if let Some(path) = player.icon.clone() {
            bar = bar.push(icon(icon::from_path(path)).size(40));
        }

        let artists = player
            .artists
            .as_ref()
            .map(|a| a.join(", "))
            .unwrap_or_else(|| "Unknown artist".to_string());
        bar = bar.push(
            column![
                text::body(player.title.clone().unwrap_or_default().into_owned())
                    .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1))),
                text::caption(artists).ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1))),
            ]
            .width(Length::Fill),
        );

        let control = |name: &'static str, enabled: bool, request: MprisRequest| {
            button::icon(icon::from_name(name).size(20).symbolic(true))
                .extra_small()
                .class(cosmic::theme::Button::AppletIcon)
                .on_press_maybe(enabled.then_some(Message::MprisRequest(request)))
        };
        let playing = player.status == PlaybackStatus::Playing;
        bar = bar
            .push(control(
                "media-skip-backward-symbolic",
                player.can_go_previous,
                MprisRequest::Previous,
            ))
            .push(if playing {
                control(
                    "media-playback-pause-symbolic",
                    player.can_pause,
                    MprisRequest::Pause,
                )
            } else {
                control(
                    "media-playback-start-symbolic",
                    player.can_play,
                    MprisRequest::Play,
                )
            })
            .push(control(
                "media-skip-forward-symbolic",
                player.can_go_next,
                MprisRequest::Next,
            ));

        Some(
            menu_button(bar)
                .on_press(Message::MprisRequest(MprisRequest::Raise))
                .padding(menu_control_padding())
                .into(),
        )
    }
}

fn section_header<'a>(label: String) -> Element<'a, Message> {
    padded_control(text::caption_heading(label))
        .padding([4, 16])
        .into()
}

fn mute_button<'a>(name: &'static str, size: u16, message: Message) -> Element<'a, Message> {
    button::icon(icon::from_name(name).size(size).symbolic(true))
        .class(cosmic::theme::Button::Icon)
        .icon_size(size)
        .on_press(message)
        .into()
}

fn percent_label<'a>(mute: bool, value: u32) -> Element<'a, Message> {
    container(text::body(if mute {
        "Muted".to_string()
    } else {
        format!("{value}%")
    }))
    .width(Length::Fixed(48.0))
    .align_x(Alignment::End)
    .into()
}

fn volume_icon(kind: Kind, mute: bool, value: u32) -> &'static str {
    match kind {
        Kind::Output if mute || value == 0 => "audio-volume-muted-symbolic",
        Kind::Output if value < 33 => "audio-volume-low-symbolic",
        Kind::Output if value < 66 => "audio-volume-medium-symbolic",
        Kind::Output if value <= 100 => "audio-volume-high-symbolic",
        Kind::Output => "audio-volume-overamplified-symbolic",
        Kind::Input if mute || value == 0 => "microphone-sensitivity-muted-symbolic",
        Kind::Input if value < 33 => "microphone-sensitivity-low-symbolic",
        Kind::Input if value < 66 => "microphone-sensitivity-medium-symbolic",
        Kind::Input => "microphone-sensitivity-high-symbolic",
    }
}

// SPDX-License-Identifier: GPL-3.0-only

use crate::backend::{self, Handle};
use crate::model::{Group, Model, percent, with_percent};
use crate::mpris_subscription::{self, MprisRequest, MprisUpdate, PlayerStatus};
use crate::pulse::{Command, Device, Kind};
use cosmic::iced::core::text::{Ellipsize, EllipsizeHeightLimit};
use cosmic::{
    Element, Task, app,
    applet::{
        menu_button,
        token::subscription::{TokenRequest, TokenUpdate, activation_token_subscription},
    },
    cctk::sctk::reexports::calloop,
    cosmic_config::{Config, ConfigGet},
    iced::{
        self, Alignment, Length, Limits, Padding, Subscription,
        futures::{SinkExt, StreamExt},
        widget::{column, row},
        window,
    },
    widget::{Column, button, container, divider, icon, scrollable, slider, space, text},
};
use mpris2_zbus::player::PlaybackStatus;

/// Height at which the app list starts scrolling.
const APP_LIST_HEIGHT: f32 = 380.0;
const POPUP_WIDTH: f32 = 400.0;
/// Left and right padding of everything in the popup.
const PAD: u16 = 20;
/// Size of the app icons, which span an app's name and slider lines.
const APP_ICON: u16 = 36;
/// Handle size for the smaller sliders (the main ones are 26).
const SMALL_HANDLE: f32 = 16.0;
const WHEEL_STEP: i32 = 5;

/// Which page of the popup is showing. Everything beyond the main page is one tap
/// away and has a back button.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Main,
    /// Output or Input devices, each with its own volume.
    Devices(Kind),
    /// Apps using the mic.
    MicApps,
    /// One app: which device it uses, and its streams.
    App(Kind, String),
}

#[derive(Default)]
pub struct Audio {
    core: cosmic::app::Core,
    popup: Option<window::Id>,
    backend: Option<Handle>,
    model: Model,
    page: Page,
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
    Show(Page),
    SetDefault(Kind, u32),
    /// Volume of the default device.
    DeviceVolume(Kind, u32),
    /// Volume of a device by its index.
    VolumeOf(Kind, u32, u32),
    ToggleDeviceMute(Kind),
    Wheel(i32),
    GroupVolume(Kind, String, u32),
    ToggleGroupMute(Kind, String),
    /// `None` is "Default"; otherwise a device index.
    GroupDevice(Kind, String, Option<u32>),
    StreamVolume(Kind, u32, u32),
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
                self.page = Page::Main;
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
            Message::Show(page) => self.page = page,

            Message::SetDefault(kind, index) => {
                if let Some(device) = self.model.devices_mut(kind).get(&index) {
                    let name = device.name.clone();
                    self.send(Command::SetDefault(kind, name));
                }
            }
            Message::DeviceVolume(kind, value) => {
                if let Some(index) = self.model.default_device(kind).map(|d| d.index) {
                    self.set_device_volume(kind, index, value);
                }
            }
            Message::VolumeOf(kind, index, value) => self.set_device_volume(kind, index, value),
            Message::Wheel(0) => {}
            Message::Wheel(delta) => {
                if let Some(device) = self.model.default_device(Kind::Output) {
                    let index = device.index;
                    let value = (percent(&device.volume) as i32 + delta)
                        .clamp(0, self.max_sink_volume as i32);
                    self.set_device_volume(Kind::Output, index, value as u32);
                }
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
                    if let Some(stream) = self.model.streams_mut(kind).get_mut(&stream.index) {
                        stream.mute = mute;
                    }
                    self.send(Command::SetStreamMute(kind, stream.index, mute));
                }
            }
            Message::GroupDevice(kind, key, index) => {
                let device = index.and_then(|i| {
                    self.model
                        .device_list(kind)
                        .into_iter()
                        .find(|d| d.index == i)
                });
                let default = self.model.default_device(kind);
                if let Some(backend) = &self.backend {
                    for stream in self.model.group_streams(kind, &key) {
                        backend.set_target(&stream, device, default);
                    }
                }
            }
            Message::StreamVolume(kind, index, value) => self.set_stream_volume(kind, index, value),

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
        let content = if !self.model.connected {
            column![padded(text::body("Connecting to the sound server…"), 8)].into()
        } else {
            match &self.page {
                Page::Main => self.main_page(),
                Page::Devices(kind) => self.devices_page(*kind),
                Page::MicApps => self.mic_apps_page(),
                Page::App(kind, key) => self.app_page(*kind, key),
            }
        };

        self.core
            .applet
            .popup_container(container(content).padding([8, 0]))
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

    fn breakpoints(&self, kind: Kind) -> &'static [u32] {
        if self.max_volume(kind) > 100 {
            &[100]
        } else {
            &[]
        }
    }

    fn set_device_volume(&mut self, kind: Kind, index: u32, value: u32) {
        if let Some(device) = self.model.devices_mut(kind).get_mut(&index) {
            device.volume = with_percent(&device.volume, value);
            let command = Command::SetDeviceVolume(kind, index, device.volume);
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

    fn output_icon_name(&self) -> &'static str {
        match self.model.default_device(Kind::Output) {
            Some(d) => volume_icon(Kind::Output, d.mute, percent(&d.volume)),
            None => "audio-volume-muted-symbolic",
        }
    }

    /// Output device and volume, input volume, the apps, then media and settings.
    fn main_page(&self) -> Element<'_, Message> {
        let output = self.model.default_device(Kind::Output);
        let input = self.model.default_device(Kind::Input);
        let (out_mute, out_value) = output.map_or((true, 0), |d| (d.mute, percent(&d.volume)));
        let (in_mute, in_value) = input.map_or((true, 0), |d| (d.mute, percent(&d.volume)));

        // The output device's name is the heading; tapping it opens the device page.
        let heading = menu_button(
            row![
                text::heading(output.map_or("No output device", |d| d.description.as_str()))
                    .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1))),
                forward_icon(),
            ]
            .align_y(Alignment::Center)
            .spacing(6),
        )
        .padding([8, PAD, 2, PAD])
        .on_press(Message::Show(Page::Devices(Kind::Output)));

        let output_row = padded(
            row![
                mute_button(
                    volume_icon(Kind::Output, out_mute, out_value),
                    20,
                    Message::ToggleDeviceMute(Kind::Output),
                ),
                slider(0..=self.max_sink_volume, out_value, |v| {
                    Message::DeviceVolume(Kind::Output, v)
                })
                .breakpoints(self.breakpoints(Kind::Output)),
                percent_label(out_mute, out_value),
            ]
            .align_y(Alignment::Center)
            .spacing(10),
            2,
        );

        // Input is one compact line, its device name trailing.
        let input_row = padded(
            row![
                mute_button(
                    volume_icon(Kind::Input, in_mute, in_value),
                    16,
                    Message::ToggleDeviceMute(Kind::Input),
                ),
                slider(0..=self.max_source_volume, in_value, |v| {
                    Message::DeviceVolume(Kind::Input, v)
                })
                .handle_width(SMALL_HANDLE)
                .handle_height(SMALL_HANDLE)
                .breakpoints(self.breakpoints(Kind::Input)),
                trailing_link(
                    input.map_or("No input".to_string(), |d| short_name(&d.description)),
                    false,
                    Message::Show(Page::Devices(Kind::Input)),
                ),
            ]
            .align_y(Alignment::Center)
            .spacing(10),
            2,
        );

        let mut page = column![heading, output_row, input_row, separator()];

        let playback = self.model.groups(Kind::Output);
        page = page.push(section_title(match playback.len() {
            0 => "No apps playing".to_string(),
            1 => "1 app playing".to_string(),
            n => format!("{n} apps playing"),
        }));
        if !playback.is_empty() {
            let list = playback.into_iter().fold(Column::new(), |list, group| {
                list.push(self.app_row(Kind::Output, group))
            });
            page = page.push(
                container(scrollable(list).height(Length::Shrink)).max_height(APP_LIST_HEIGHT),
            );
        }

        let record = self.model.groups(Kind::Input);
        if !record.is_empty() {
            let names: Vec<&str> = record.iter().map(|g| g.name).collect();
            page = page.push(
                menu_button(
                    row![
                        icon::from_name("audio-input-microphone-symbolic")
                            .size(14)
                            .symbolic(true)
                            .icon(),
                        text::caption(format!("{} using mic", names.join(", ")))
                            .width(Length::Fill)
                            .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1))),
                        forward_icon(),
                    ]
                    .align_y(Alignment::Center)
                    .spacing(10),
                )
                .padding([6, PAD])
                .on_press(Message::Show(Page::MicApps)),
            );
        }

        if let Some(player) = self.media_bar() {
            page = page.push(separator()).push(player);
        }

        page.push(separator())
            .push(
                menu_button(text::body("Sound settings…"))
                    .padding([8, PAD])
                    .on_press(Message::OpenSettings),
            )
            .into()
    }

    /// One app: a large icon (click to mute) beside its name, where it plays, and a
    /// wide slider. Tapping where it plays opens the app's page.
    fn app_row<'a>(&'a self, kind: Kind, group: Group<'a>) -> Element<'a, Message> {
        let key = group.key.to_string();
        let value = group.percent();
        let muted = group.muted();
        let pinned = group
            .streams
            .first()
            .and_then(|s| self.model.pinned_device(s));

        let mut where_to = pinned.map_or("Default".to_string(), |d| short_name(&d.description));
        if group.streams.len() > 1 {
            where_to = format!("{where_to} · {} streams", group.streams.len());
        }

        let name_line = row![
            text::body(group.name)
                .width(Length::Fill)
                .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1))),
            trailing_link(
                where_to,
                pinned.is_some(),
                Message::Show(Page::App(kind, key.clone())),
            ),
        ]
        .align_y(Alignment::Center)
        .spacing(6)
        // Start the name where the slider's rail starts, past half the handle.
        .padding(Padding::default().left(SMALL_HANDLE / 2.0));

        let slider_line = row![
            slider(0..=self.max_volume(kind), value, {
                let key = key.clone();
                move |v| Message::GroupVolume(kind, key.clone(), v)
            })
            .handle_width(SMALL_HANDLE)
            .handle_height(SMALL_HANDLE),
            small_percent(muted, value),
        ]
        .align_y(Alignment::Center)
        .spacing(8);

        padded(
            row![
                app_icon_button(
                    group.icon_name,
                    kind,
                    muted,
                    APP_ICON,
                    Message::ToggleGroupMute(kind, key.clone()),
                ),
                column![name_line, slider_line]
                    .spacing(2)
                    .width(Length::Fill),
            ]
            .align_y(Alignment::Center)
            .spacing(12),
            6,
        )
    }

    /// Every output (or input) device with a tick on the default and its own volume.
    fn devices_page(&self, kind: Kind) -> Element<'_, Message> {
        let default = self.model.default_device(kind);
        let mut page = column![
            back_header(
                match kind {
                    Kind::Output => "Output",
                    Kind::Input => "Input",
                },
                None
            ),
            section_title("Device".to_string()),
        ];

        for device in self.model.device_list(kind) {
            let selected = default.is_some_and(|d| d.index == device.index);
            page = page.push(choice(
                &device.description,
                None,
                selected,
                Message::SetDefault(kind, device.index),
            ));
            page = page.push(self.device_slider(kind, device));
        }
        page.into()
    }

    fn device_slider<'a>(&self, kind: Kind, device: &Device) -> Element<'a, Message> {
        let index = device.index;
        let value = percent(&device.volume);
        container(
            row![
                slider(0..=self.max_volume(kind), value, move |v| {
                    Message::VolumeOf(kind, index, v)
                })
                .handle_width(SMALL_HANDLE)
                .handle_height(SMALL_HANDLE),
                small_percent(device.mute, value),
            ]
            .align_y(Alignment::Center)
            .spacing(8),
        )
        .padding(
            Padding::from([0, PAD])
                .left(f32::from(PAD) + CHOICE_INDENT - SMALL_HANDLE / 2.0)
                .bottom(8),
        )
        .into()
    }

    fn mic_apps_page(&self) -> Element<'_, Message> {
        let mut page = column![back_header("Using mic", None)];
        for group in self.model.groups(Kind::Input) {
            page = page.push(self.app_row(Kind::Input, group));
        }
        page.into()
    }

    /// Where an app plays (or records from), and its streams.
    fn app_page<'a>(&'a self, kind: Kind, key: &str) -> Element<'a, Message> {
        let Some(group) = self.model.groups(kind).into_iter().find(|g| g.key == key) else {
            // The app stopped while its page was open.
            return column![
                back_header("App", None),
                section_title("Not playing any more".to_string())
            ]
            .into();
        };

        let pinned = group
            .streams
            .first()
            .and_then(|s| self.model.pinned_device(s));
        let key = key.to_string();

        let mut page = column![
            back_header(group.name, Some(group.icon_name)),
            section_title(
                match kind {
                    Kind::Output => "Plays on",
                    Kind::Input => "Records from",
                }
                .to_string()
            ),
            choice(
                "Default",
                Some(match kind {
                    Kind::Output => "follows Output",
                    Kind::Input => "follows Input",
                }),
                pinned.is_none(),
                Message::GroupDevice(kind, key.clone(), None),
            ),
        ];
        for device in self.model.device_list(kind) {
            page = page.push(choice(
                &device.description,
                None,
                pinned.is_some_and(|p| p.index == device.index),
                Message::GroupDevice(kind, key.clone(), Some(device.index)),
            ));
        }

        if group.streams.len() > 1 {
            page = page
                .push(separator())
                .push(section_title("Streams".to_string()));
            let max = self.max_volume(kind);
            let streams = group
                .streams
                .iter()
                .fold(Column::new().spacing(8), |list, stream| {
                    let index = stream.index;
                    let value = percent(&stream.volume);
                    list.push(column![
                        // Start the name where the slider's rail starts.
                        row![
                            text::body(stream.media_name.as_str())
                                .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)))
                        ]
                        .padding(Padding::default().left(SMALL_HANDLE / 2.0)),
                        row![
                            slider(0..=max, value, move |v| Message::StreamVolume(
                                kind, index, v
                            ))
                            .handle_width(SMALL_HANDLE)
                            .handle_height(SMALL_HANDLE),
                            small_percent(stream.mute, value),
                        ]
                        .align_y(Alignment::Center)
                        .spacing(8),
                    ])
                });
            // Indented to line up with the device names above.
            page = page.push(
                container(streams.width(Length::Fill)).padding(
                    Padding::from([4, PAD])
                        .left(f32::from(PAD) + CHOICE_INDENT - SMALL_HANDLE / 2.0)
                        .bottom(8),
                ),
            );
        }
        page.into()
    }

    fn media_bar(&self) -> Option<Element<'_, Message>> {
        let player = self.player.as_ref()?;

        let mut bar = cosmic::widget::Row::new()
            .align_y(Alignment::Center)
            .spacing(12);
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
            button::icon(icon::from_name(name).size(18).symbolic(true))
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
                .padding([6, PAD])
                .into(),
        )
    }
}

/// How far device names in a choice list sit from the edge: past the tick.
const CHOICE_INDENT: f32 = 28.0;

/// Content padded to the popup's side margins.
fn padded<'a>(content: impl Into<Element<'a, Message>>, vertical: u16) -> Element<'a, Message> {
    container(content).padding([vertical, PAD]).into()
}

fn separator<'a>() -> Element<'a, Message> {
    container(divider::horizontal::default())
        .padding([6, PAD])
        .into()
}

fn section_title<'a>(label: String) -> Element<'a, Message> {
    container(text::caption_heading(label))
        .padding([6, PAD, 2, PAD])
        .into()
}

fn forward_icon<'a>() -> Element<'a, Message> {
    icon::from_name("go-next-symbolic")
        .size(14)
        .symbolic(true)
        .icon()
        .into()
}

/// Quiet text with a chevron, e.g. "Default ›"; accent-coloured when `highlight`.
fn trailing_link<'a>(label: String, highlight: bool, message: Message) -> Element<'a, Message> {
    let label = text::caption(label).ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)));
    let label = if highlight {
        label.class(cosmic::theme::Text::Accent)
    } else {
        label
    };
    button::custom(
        row![
            label,
            icon::from_name("go-next-symbolic")
                .size(12)
                .symbolic(true)
                .icon()
        ]
        .align_y(Alignment::Center)
        .spacing(2),
    )
    .class(cosmic::theme::Button::MenuItem)
    .padding([2, 6])
    .on_press(message)
    .into()
}

/// "‹ Firefox" at the top of a detail page.
fn back_header<'a>(title: &'a str, app_icon: Option<Option<&'a str>>) -> Element<'a, Message> {
    let mut line = row![
        icon::from_name("go-previous-symbolic")
            .size(16)
            .symbolic(true)
            .icon()
    ]
    .align_y(Alignment::Center)
    .spacing(10);
    if let Some(icon_name) = app_icon {
        line = line.push(app_icon_image(icon_name, 24));
    }
    line =
        line.push(text::heading(title).ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1))));
    menu_button(line)
        .padding([8, PAD - 4])
        .on_press(Message::Show(Page::Main))
        .into()
}

/// A line in a device list, ticked when `selected`.
fn choice<'a>(
    label: &str,
    hint: Option<&'static str>,
    selected: bool,
    message: Message,
) -> Element<'a, Message> {
    let tick: Element<'a, Message> = if selected {
        icon::from_name("object-select-symbolic")
            .size(16)
            .symbolic(true)
            .icon()
            .into()
    } else {
        space::horizontal().width(Length::Fixed(16.0)).into()
    };
    let mut line = row![
        tick,
        text::body(label.to_string()).ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)))
    ]
    .align_y(Alignment::Center)
    .spacing(CHOICE_INDENT - 16.0);
    if let Some(hint) = hint {
        line = line.push(text::caption(hint));
    }
    menu_button(line).padding([8, PAD]).on_press(message).into()
}

fn app_icon_image<'a>(icon_name: Option<&str>, size: u16) -> Element<'a, Message> {
    icon::from_name(icon_name.unwrap_or("application-x-executable"))
        .size(size)
        .fallback(Some(icon::IconFallback::Names(vec![
            "application-x-executable".into(),
            "audio-x-generic".into(),
        ])))
        .icon()
        .into()
}

/// An app's icon as its mute button; a muted app shows the muted icon instead.
fn app_icon_button<'a>(
    icon_name: Option<&str>,
    kind: Kind,
    muted: bool,
    size: u16,
    message: Message,
) -> Element<'a, Message> {
    let image = if muted {
        icon::from_name(volume_icon(kind, true, 0))
            .size(size * 2 / 3)
            .symbolic(true)
            .icon()
            .into()
    } else {
        app_icon_image(icon_name, size)
    };
    button::custom(
        container(image)
            .width(Length::Fixed(f32::from(size)))
            .height(Length::Fixed(f32::from(size)))
            .align_x(Alignment::Center)
            .align_y(Alignment::Center),
    )
    .class(cosmic::theme::Button::Icon)
    .padding(2)
    .on_press(message)
    .into()
}

/// "Headset (Arctis 7)" → "Headset", to keep trailing labels short.
fn short_name(description: &str) -> String {
    description
        .split(" (")
        .next()
        .unwrap_or(description)
        .to_string()
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

fn small_percent<'a>(mute: bool, value: u32) -> Element<'a, Message> {
    container(text::caption(if mute {
        "Muted".to_string()
    } else {
        format!("{value}%")
    }))
    .width(Length::Fixed(40.0))
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

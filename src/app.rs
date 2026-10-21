// SPDX-License-Identifier: GPL-3.0-only

use crate::backend::{self, Handle};
use crate::model::{Group, Model, percent, with_percent};
use crate::mpris_subscription::{self, MprisRequest, MprisUpdate, PlayerStatus};
use crate::pulse::{Command, Kind};
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
    theme,
    widget::{
        Column, button, container, divider, icon, list_column, scrollable, slider, space, text,
    },
};
use mpris2_zbus::player::PlaybackStatus;

/// Height at which the app lists start scrolling.
const PLAYBACK_LIST_HEIGHT: f32 = 340.0;
const RECORD_LIST_HEIGHT: f32 = 170.0;
const POPUP_WIDTH: f32 = 400.0;
const APP_NAME_WIDTH: f32 = 180.0;
/// Handle size for the app sliders, smaller than the main ones (26).
const SMALL_HANDLE: f32 = 16.0;
/// Width of the app-icon column in app rows (24px icon plus padding); the ticks and
/// stream icons below an app sit in this column so everything lines up.
const ICON_COLUMN: f32 = 32.0;
const ROW_SPACING: f32 = 6.0;
const WHEEL_STEP: i32 = 5;

/// The one list that is unfolded in the popup, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Open {
    /// The device list under Output or Input.
    Devices(Kind),
    /// An app's device list and per-stream sliders.
    App(Kind, String),
}

#[derive(Default)]
pub struct Audio {
    core: cosmic::app::Core,
    popup: Option<window::Id>,
    backend: Option<Handle>,
    model: Model,
    open: Option<Open>,
    /// Whether the apps playing sound are listed; folded by default.
    apps_open: bool,
    /// Whether the apps using the mic are listed.
    mics_open: bool,
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
    Toggle(Open),
    ToggleApps,
    ToggleMics,
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

            Message::SetDefault(kind, pos) => {
                self.open = None;
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
                self.open = None;
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
            Message::Toggle(open) => {
                self.open = (self.open.as_ref() != Some(&open)).then_some(open);
            }
            Message::ToggleApps => self.apps_open = !self.apps_open,
            Message::ToggleMics => self.mics_open = !self.mics_open,

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
            // Volumes first, then which devices they apply to, then the apps.
            content = content
                .push(self.volume_row(Kind::Output))
                .push(self.volume_row(Kind::Input))
                .push(separator())
                .push(self.device_choice(Kind::Output))
                .push(self.device_choice(Kind::Input));

            let playback = self.model.groups(Kind::Output);
            let record = self.model.groups(Kind::Input);
            if !playback.is_empty() || !record.is_empty() {
                content = content.push(separator());
            }
            if !playback.is_empty() {
                content = content.push(summary_line(
                    "audio-speakers-symbolic",
                    summary(&playback, "playing"),
                    self.apps_open,
                    Message::ToggleApps,
                ));
                if self.apps_open {
                    content =
                        content.push(self.app_list(Kind::Output, playback, PLAYBACK_LIST_HEIGHT));
                }
            }
            if !record.is_empty() {
                content = content.push(summary_line(
                    "audio-input-microphone-symbolic",
                    summary(&record, "using mic"),
                    self.mics_open,
                    Message::ToggleMics,
                ));
                if self.mics_open {
                    content = content.push(self.app_list(Kind::Input, record, RECORD_LIST_HEIGHT));
                }
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

    /// The Output or Input volume: mute button, slider and percentage.
    fn volume_row(&self, kind: Kind) -> Element<'_, Message> {
        let (mute, value) = self
            .model
            .default_device(kind)
            .map_or((true, 0), |d| (d.mute, percent(&d.volume)));

        padded_control(
            row![
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
            .spacing(10),
        )
        .into()
    }

    /// "Output  Speakers (Built-in)  ⌄": which device the volume applies to, with the
    /// device list unfolded under it when open.
    fn device_choice(&self, kind: Kind) -> Element<'_, Message> {
        let devices = self.model.device_list(kind);
        let default = self.model.default_device(kind);
        let open = self.open == Some(Open::Devices(kind));

        let heading = menu_button(
            row![
                icon::from_name(match kind {
                    Kind::Output => "audio-speakers-symbolic",
                    Kind::Input => "audio-input-microphone-symbolic",
                })
                .size(16)
                .symbolic(true)
                .icon(),
                text::body(match kind {
                    Kind::Output => "Output",
                    Kind::Input => "Input",
                })
                .width(Length::Fixed(56.0)),
                text::body(default.map_or("No device", |d| d.description.as_str()))
                    .class(cosmic::theme::Text::Accent)
                    .width(Length::Fill)
                    .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1))),
                chevron(open),
            ]
            .align_y(Alignment::Center)
            .spacing(10),
        )
        .on_press(Message::Toggle(Open::Devices(kind)));

        let mut section = column![heading];
        if open {
            for (pos, device) in devices.iter().enumerate() {
                let selected = default.is_some_and(|d| d.index == device.index);
                section = section.push(
                    menu_button(
                        row![
                            tick(selected),
                            text::body(device.description.as_str())
                                .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1))),
                        ]
                        .align_y(Alignment::Center)
                        .spacing(10),
                    )
                    // Line the device names up under the current device's name.
                    .padding([6, 16, 6, 90])
                    .on_press(Message::SetDefault(kind, pos)),
                );
            }
        }
        section.into()
    }

    /// Apps in a card, one line each, scrolling once the card is `height` tall.
    fn app_list<'a>(
        &'a self,
        kind: Kind,
        groups: Vec<Group<'a>>,
        height: f32,
    ) -> Element<'a, Message> {
        let list = groups
            .into_iter()
            .fold(list_column().list_item_padding([2, 8]), |list, group| {
                list.add(self.app_row(kind, group))
            });

        container(container(scrollable(list).height(Length::Shrink)).max_height(height))
            .padding([4, 12])
            .into()
    }

    fn app_row<'a>(&'a self, kind: Kind, group: Group<'a>) -> Element<'a, Message> {
        let key = group.key.to_string();
        let value = group.percent();
        let muted = group.muted();
        let max = self.max_volume(kind);
        let open = self.open == Some(Open::App(kind, key.clone()));

        // The app's icon is its mute button; a muted app shows the muted icon instead.
        let icon_element: Element<'a, Message> = if muted {
            icon::from_name(volume_icon(kind, true, 0))
                .size(24)
                .symbolic(true)
                .icon()
                .into()
        } else {
            icon::from_name(group.icon_name.unwrap_or("application-x-executable"))
                .size(24)
                .fallback(Some(icon::IconFallback::Names(vec![
                    "application-x-executable".into(),
                    "audio-x-generic".into(),
                ])))
                .icon()
                .into()
        };
        let mute = button::custom(icon_element)
            .class(cosmic::theme::Button::Icon)
            .padding(4)
            .width(Length::Fixed(ICON_COLUMN))
            .on_press(Message::ToggleGroupMute(kind, key.clone()));

        // Every stream of the app is pinned to the same device, or the first one decides.
        let pinned = group
            .streams
            .first()
            .and_then(|s| self.model.pinned_device(s));

        // Name, stream count and pinned-device tag share one fixed-width button that
        // opens the device list, so every slider starts at the same place.
        let mut name = row![
            text::body(group.name)
                .width(Length::Fill)
                .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)))
        ]
        .align_y(Alignment::Center)
        .spacing(6);
        if group.streams.len() > 1 {
            name = name.push(text::caption(group.streams.len().to_string()));
        }
        if let Some(device) = pinned {
            name = name.push(
                text::caption(short_name(&device.description)).class(cosmic::theme::Text::Accent),
            );
        }
        let name = button::custom(name)
            .class(cosmic::theme::Button::MenuItem)
            .padding([4, 6])
            .width(Length::Fixed(APP_NAME_WIDTH))
            .on_press(Message::Toggle(Open::App(kind, key.clone())));

        let mut line = row![mute, name]
            .align_y(Alignment::Center)
            .spacing(ROW_SPACING);
        line = line.push(
            slider(0..=max, value, {
                let key = key.clone();
                move |v| Message::GroupVolume(kind, key.clone(), v)
            })
            .handle_width(SMALL_HANDLE)
            .handle_height(SMALL_HANDLE),
        );

        let mut item = column![line];
        if open {
            let follows = match kind {
                Kind::Output => "follows Output",
                Kind::Input => "follows Input",
            };
            item = item.push(app_option(
                "Default",
                Some(follows),
                pinned.is_none(),
                Message::GroupDevice(kind, key.clone(), 0),
            ));
            for (pos, device) in self.model.device_list(kind).iter().enumerate() {
                item = item.push(app_option(
                    &device.description,
                    None,
                    pinned.is_some_and(|p| p.index == device.index),
                    Message::GroupDevice(kind, key.clone(), pos + 1),
                ));
            }

            if group.streams.len() > 1 {
                item = item.push(padded_control(divider::horizontal::default()).padding([4, 0]));
                for stream in &group.streams {
                    let value = percent(&stream.volume);
                    let index = stream.index;
                    let mute = button::icon(
                        icon::from_name(volume_icon(kind, stream.mute, value))
                            .size(12)
                            .symbolic(true),
                    )
                    .class(cosmic::theme::Button::Icon)
                    .icon_size(12)
                    .padding(4)
                    .on_press(Message::ToggleStreamMute(kind, index));
                    item = item.push(aligned_row(
                        mute.into(),
                        text::caption(stream.media_name.as_str())
                            .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)))
                            .into(),
                        Some(
                            slider(0..=max, value, move |v| {
                                Message::StreamVolume(kind, index, v)
                            })
                            .handle_width(SMALL_HANDLE)
                            .handle_height(SMALL_HANDLE)
                            .into(),
                        ),
                    ));
                }
            }
        }

        item.into()
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

fn chevron<'a>(open: bool) -> Element<'a, Message> {
    icon::from_name(if open {
        "go-up-symbolic"
    } else {
        "go-down-symbolic"
    })
    .size(16)
    .symbolic(true)
    .icon()
    .into()
}

fn tick<'a>(selected: bool) -> Element<'a, Message> {
    if selected {
        icon::from_name("object-select-symbolic")
            .size(16)
            .symbolic(true)
            .icon()
            .into()
    } else {
        space::horizontal().width(Length::Fixed(16.0)).into()
    }
}

/// A row laid out like an app's line: something in the icon column, a label in the
/// name column, then optionally a slider, so rows under an app line up with it.
fn aligned_row<'a>(
    lead: Element<'a, Message>,
    label: Element<'a, Message>,
    trailing: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut line = row![
        container(lead)
            .width(Length::Fixed(ICON_COLUMN))
            .align_x(Alignment::Center),
        container(label)
            .width(Length::Fixed(APP_NAME_WIDTH))
            .padding([0, 6]),
    ]
    .align_y(Alignment::Center)
    .spacing(ROW_SPACING);
    if let Some(trailing) = trailing {
        line = line.push(trailing);
    }
    line.into()
}

/// A device choice under an app, ticked when `selected`.
fn app_option<'a>(
    label: &str,
    hint: Option<&'static str>,
    selected: bool,
    message: Message,
) -> Element<'a, Message> {
    let mut text_row = row![
        text::body(label.to_string()).ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)))
    ]
    .align_y(Alignment::Center)
    .spacing(8);
    if let Some(hint) = hint {
        text_row = text_row.push(text::caption(hint));
    }
    menu_button(aligned_row(tick(selected), text_row.into(), None))
        .padding([5, 0])
        .on_press(message)
        .into()
}

/// A folded section heading: icon, summary text and a chevron.
fn summary_line<'a>(
    icon_name: &'static str,
    label: String,
    open: bool,
    message: Message,
) -> Element<'a, Message> {
    menu_button(
        row![
            icon::from_name(icon_name).size(16).symbolic(true).icon(),
            text::body(label)
                .width(Length::Fill)
                .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1))),
            chevron(open),
        ]
        .align_y(Alignment::Center)
        .spacing(10),
    )
    .on_press(message)
    .into()
}

/// "Firefox, Spotify, Discord +6 playing"
fn summary(groups: &[Group<'_>], verb: &str) -> String {
    const SHOWN: usize = 3;
    let names: Vec<&str> = groups.iter().take(SHOWN).map(|g| g.name).collect();
    match groups.len().saturating_sub(SHOWN) {
        0 => format!("{} {verb}", names.join(", ")),
        more => format!("{} +{more} {verb}", names.join(", ")),
    }
}

/// "Headset (Arctis 7)" → "Headset", to keep the pinned-device tag short.
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

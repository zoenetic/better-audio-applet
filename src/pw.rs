// SPDX-License-Identifier: GPL-3.0-only

//! PipeWire `default` metadata: which device each stream is pinned to.
//!
//! WirePlumber follows the `target.object` key to link a stream, and remembers it
//! per application, so setting it pins an app to a device and removing it returns
//! the app to following the default device. When it restores a remembered device,
//! older WirePlumber versions write `target.node` instead.

use futures::channel::mpsc::UnboundedSender;
use pipewire::metadata::{Metadata, MetadataListener};
use pipewire::registry::GlobalObject;
use pipewire::types::ObjectType;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::backend::{Event, TargetKey};

#[derive(Debug)]
pub enum Request {
    /// Pin a stream node to a device by its `object.serial`, or `None` to follow the default.
    SetTarget {
        subject: u32,
        serial: Option<String>,
    },
}

pub type SharedSender = Arc<Mutex<Option<pipewire::channel::Sender<Request>>>>;

pub fn run(sender: SharedSender, events: UnboundedSender<Event>) {
    pipewire::init();
    loop {
        let (tx, rx) = pipewire::channel::channel();
        *sender.lock().unwrap() = Some(tx);
        if let Err(why) = session(rx, &events) {
            tracing::warn!("pipewire session ended: {why}");
        }
        *sender.lock().unwrap() = None;
        _ = events.unbounded_send(Event::TargetsReset);
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn session(
    rx: pipewire::channel::Receiver<Request>,
    events: &UnboundedSender<Event>,
) -> Result<(), pipewire::Error> {
    let main_loop = pipewire::main_loop::MainLoopRc::new(None)?;
    let context = pipewire::context::ContextRc::new(&main_loop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;

    let metadata: Rc<RefCell<Option<(Metadata, MetadataListener)>>> = Rc::new(RefCell::new(None));

    let _requests = rx.attach(main_loop.loop_(), {
        let metadata = Rc::clone(&metadata);
        move |request| {
            let Request::SetTarget { subject, serial } = request;
            if let Some((metadata, _)) = metadata.borrow().as_ref() {
                // Older pipewire-pulse versions also wrote `target.node`.
                metadata.set_property(subject, "target.node", None, None);
                match serial {
                    Some(serial) => metadata.set_property(
                        subject,
                        "target.object",
                        Some("Spa:Id"),
                        Some(&serial),
                    ),
                    None => metadata.set_property(subject, "target.object", None, None),
                }
            }
        }
    });

    // Quit (and reconnect) if the PipeWire connection drops.
    let _core_listener = core
        .add_listener_local()
        .error({
            let main_loop = main_loop.downgrade();
            move |id, _seq, res, message| {
                if id == pipewire::core::PW_ID_CORE {
                    tracing::warn!("pipewire core error {res}: {message}");
                    if let Some(main_loop) = main_loop.upgrade() {
                        main_loop.quit();
                    }
                }
            }
        })
        .register();

    let registry_weak = registry.downgrade();
    let _registry_listener = registry
        .add_listener_local()
        .global({
            let metadata = Rc::clone(&metadata);
            let events = events.clone();
            move |obj| {
                if obj.type_ != ObjectType::Metadata || !is_default_metadata(obj) {
                    return;
                }
                let Some(registry) = registry_weak.upgrade() else {
                    return;
                };
                let Ok(proxy) = registry.bind::<Metadata, _>(obj) else {
                    return;
                };
                let events = events.clone();
                let listener = proxy
                    .add_listener_local()
                    .property(move |subject, key, _type, value| {
                        let key = match key {
                            Some("target.object") => Some(TargetKey::Object),
                            Some("target.node") => Some(TargetKey::Node),
                            None => None,
                            Some(_) => return 0,
                        };
                        let value = value
                            .filter(|v| !v.is_empty() && *v != "-1")
                            .map(str::to_owned);
                        _ = events.unbounded_send(Event::Target(subject, key, value));
                        0
                    })
                    .register();
                *metadata.borrow_mut() = Some((proxy, listener));
            }
        })
        .register();

    main_loop.run();
    Ok(())
}

fn is_default_metadata<P: AsRef<pipewire::spa::utils::dict::DictRef>>(
    obj: &GlobalObject<P>,
) -> bool {
    obj.props
        .as_ref()
        .and_then(|props| props.as_ref().get("metadata.name"))
        == Some("default")
}

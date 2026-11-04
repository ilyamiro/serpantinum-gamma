use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use calloop::ping::Ping;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::Value;
use zbus::{Connection, interface};

use crate::color::Color;
use crate::model::{Model, Target};

pub const BUS_NAME: &str = "rs.wl-gammarelay";
pub const INTERFACE: &str = "rs.wl.gammarelay";

pub type Shared = Arc<Mutex<Model>>;

/// The `rs.wl.gammarelay` interface on `/` or on one `/outputs/<name>`
pub struct Relay {
    model: Shared,
    target: Target,
    path: String,
    connection: Connection,
    wake: Ping,
}

impl Relay {
    pub fn new(model: Shared, target: Target, path: String, connection: Connection, wake: Ping) -> Self {
        Relay { model, target, path, connection, wake }
    }

    fn color(&self) -> Color {
        self.model.lock().unwrap().color(self.target).unwrap_or_default()
    }

    /// Applies `change`, wakes the Wayland side to send the new ramps, and signals every object
    /// whose values moved. A property setter skips its own object, which zbus signals itself.
    async fn change(&self, from_setter: bool, change: impl FnOnce(&mut Model, Target)) {
        let (before, after) = {
            let mut model = self.model.lock().unwrap();
            let before = model.snapshot();
            change(&mut model, self.target);
            (before, model.snapshot())
        };
        self.wake.ping();
        let skip = from_setter.then_some(self.path.as_str());
        emit_changes(&self.connection, &before, &after, skip).await;
    }
}

#[interface(name = "rs.wl.gammarelay")]
impl Relay {
    async fn toggle_inverted(&self) {
        self.change(false, |model, target| model.toggle_inverted(target)).await;
    }

    async fn update_temperature(&self, delta: i16) {
        self.change(false, |model, target| {
            model.update(target, |c| c.with_temperature(c.temperature.saturating_add_signed(delta)))
        })
        .await;
    }

    async fn update_brightness(&self, delta: f64) {
        self.change(false, |model, target| model.update(target, |c| c.with_brightness(c.brightness + delta))).await;
    }

    async fn update_gamma(&self, delta: f64) {
        self.change(false, |model, target| model.update(target, |c| c.with_gamma(c.gamma + delta))).await;
    }

    #[zbus(property)]
    fn temperature(&self) -> u16 {
        self.color().temperature
    }

    #[zbus(property)]
    async fn set_temperature(&self, value: u16) {
        self.change(true, |model, target| model.update(target, |c| c.with_temperature(value))).await;
    }

    #[zbus(property)]
    fn brightness(&self) -> f64 {
        self.color().brightness
    }

    #[zbus(property)]
    async fn set_brightness(&self, value: f64) {
        self.change(true, |model, target| model.update(target, |c| c.with_brightness(value))).await;
    }

    #[zbus(property)]
    fn gamma(&self) -> f64 {
        self.color().gamma
    }

    #[zbus(property)]
    async fn set_gamma(&self, value: f64) {
        self.change(true, |model, target| model.update(target, |c| c.with_gamma(value))).await;
    }

    #[zbus(property)]
    fn inverted(&self) -> bool {
        self.color().inverted
    }

    #[zbus(property)]
    async fn set_inverted(&self, value: bool) {
        self.change(true, |model, target| model.update(target, |c| Color { inverted: value, ..c })).await;
    }
}

fn changed_properties(old: &Color, new: &Color) -> HashMap<&'static str, Value<'static>> {
    let mut changed = HashMap::new();
    if old.temperature != new.temperature {
        changed.insert("Temperature", Value::from(new.temperature));
    }
    if old.brightness != new.brightness {
        changed.insert("Brightness", Value::from(new.brightness));
    }
    if old.gamma != new.gamma {
        changed.insert("Gamma", Value::from(new.gamma));
    }
    if old.inverted != new.inverted {
        changed.insert("Inverted", Value::from(new.inverted));
    }
    changed
}

async fn emit_changes(
    connection: &Connection,
    before: &[(String, Color)],
    after: &[(String, Color)],
    skip: Option<&str>,
) {
    // Both snapshots were taken under one lock, so they list the same objects in the same order
    for ((path, old), (_, new)) in before.iter().zip(after) {
        if skip == Some(path.as_str()) {
            continue;
        }
        let changed = changed_properties(old, new);
        if changed.is_empty() {
            continue;
        }
        let Ok(emitter) = SignalEmitter::new(connection, path.as_str()) else { continue };
        let interface = INTERFACE.try_into().expect("valid interface name");
        if let Err(error) =
            zbus::fdo::Properties::properties_changed(&emitter, interface, changed, Cow::Borrowed(&[])).await
        {
            eprintln!("serpantinum-gamma: could not signal a change on {path}: {error}");
        }
    }
}

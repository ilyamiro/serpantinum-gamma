use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use calloop::ping::Ping;
use zbus::zvariant::Value;
use zbus::{Connection, fdo, interface};

use crate::clock::{self, Now};
use crate::color::{Color, TEMP_MAX, TEMP_MIN};
use crate::model::{Model, Target};
use crate::schedule::{Location, MINUTES_PER_DAY, View};

pub const BUS_NAME: &str = "org.serpantinum.Gamma";
pub const INTERFACE: &str = "org.serpantinum.Gamma";
pub const SCHEDULE_INTERFACE: &str = "org.serpantinum.Gamma.Schedule";
const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";

pub type Shared = Arc<Mutex<Model>>;

/// Everything the objects show, captured to tell which properties a change moved
pub struct State {
    colors: Vec<(String, Color)>,
    schedule: View,
}

impl State {
    pub fn capture(model: &Model, now: &Now) -> State {
        State { colors: model.snapshot(), schedule: model.schedule.view(now) }
    }
}

/// The property a setter changed, which zbus signals itself
#[derive(Clone, Copy)]
pub enum Skip<'a> {
    Nothing,
    Color(&'a str),
    Schedule(&'static str),
}

/// A `PropertiesChanged` signal to send: the object path, the interface and the changed values
pub type Signal = (String, &'static str, HashMap<&'static str, Value<'static>>);

/// Runs `change` and the schedule on the model, and returns the signals for what moved
fn apply(model: &Shared, skip: Skip, change: impl FnOnce(&mut Model)) -> Vec<Signal> {
    let now = clock::now();
    let mut model = model.lock().unwrap();
    let mut before = State::capture(&model, &now);
    if let Some(signalled) = model.signalled {
        before.schedule = signalled;
    }
    change(&mut model);
    model.apply_schedule(&now);
    let after = State::capture(&model, &now);
    model.signalled = Some(after.schedule);
    signals(&before, &after, skip)
}

/// Applies the schedule as time passes; called from the event loop
pub fn tick(model: &Shared, bus: &zbus::blocking::Connection) {
    for (path, interface, changed) in apply(model, Skip::Nothing, |_| {}) {
        let body = (interface, changed, Vec::<&str>::new());
        if let Err(error) =
            bus.emit_signal(None::<&str>, path.as_str(), PROPERTIES_INTERFACE, "PropertiesChanged", &body)
        {
            eprintln!("serpantinum-gamma: could not signal a change on {path}: {error}");
        }
    }
}

async fn send(connection: &Connection, signals: Vec<Signal>) {
    for (path, interface, changed) in signals {
        let body = (interface, changed, Vec::<&str>::new());
        if let Err(error) =
            connection.emit_signal(None::<&str>, path.as_str(), PROPERTIES_INTERFACE, "PropertiesChanged", &body).await
        {
            eprintln!("serpantinum-gamma: could not signal a change on {path}: {error}");
        }
    }
}

/// The `org.serpantinum.Gamma` interface on `/` or on one `/outputs/<name>`
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
    /// whose values moved. A property setter skips its own property, which zbus signals itself.
    async fn change(&self, from_setter: bool, change: impl FnOnce(&mut Model, Target)) {
        let skip = if from_setter { Skip::Color(&self.path) } else { Skip::Nothing };
        let signals = apply(&self.model, skip, |model| change(model, self.target));
        self.wake.ping();
        send(&self.connection, signals).await;
    }

    /// A temperature set by hand turns the schedule off
    async fn change_temperature(&self, from_setter: bool, change: impl Fn(Color) -> Color) {
        self.change(from_setter, |model, target| {
            model.schedule.auto = false;
            model.update(target, change);
        })
        .await;
    }
}

#[interface(name = "org.serpantinum.Gamma")]
impl Relay {
    async fn toggle_inverted(&self) {
        self.change(false, |model, target| model.toggle_inverted(target)).await;
    }

    async fn update_temperature(&self, delta: i16) {
        self.change_temperature(false, |c| c.with_temperature(c.temperature.saturating_add_signed(delta))).await;
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
        self.change_temperature(true, |c| c.with_temperature(value)).await;
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

/// The `org.serpantinum.Gamma.Schedule` interface on `/`: one day and night schedule for every output
pub struct ScheduleRelay {
    model: Shared,
    connection: Connection,
    wake: Ping,
}

impl ScheduleRelay {
    pub fn new(model: Shared, connection: Connection, wake: Ping) -> Self {
        ScheduleRelay { model, connection, wake }
    }

    fn view(&self) -> View {
        self.model.lock().unwrap().schedule.view(&clock::now())
    }

    async fn change(&self, skip: Option<&'static str>, change: impl FnOnce(&mut Model)) {
        let signals = apply(&self.model, skip.map_or(Skip::Nothing, Skip::Schedule), change);
        self.wake.ping();
        send(&self.connection, signals).await;
    }
}

fn minutes_of_day(value: u16) -> fdo::Result<u16> {
    if value < MINUTES_PER_DAY {
        Ok(value)
    } else {
        Err(fdo::Error::InvalidArgs(format!("{value} is not a minute of the day, 0 to {}", MINUTES_PER_DAY - 1)))
    }
}

#[interface(name = "org.serpantinum.Gamma.Schedule")]
impl ScheduleRelay {
    async fn set_location(&self, latitude: f64, longitude: f64) -> fdo::Result<()> {
        let location = Location::new(latitude, longitude).ok_or_else(|| {
            fdo::Error::InvalidArgs(format!(
                "({latitude}, {longitude}) is not a latitude in -90..90 and a longitude in -180..180"
            ))
        })?;
        self.change(None, |model| model.schedule.location = Some(location)).await;
        Ok(())
    }

    async fn clear_location(&self) {
        self.change(None, |model| model.schedule.location = None).await;
    }

    #[zbus(property)]
    fn auto(&self) -> bool {
        self.view().auto
    }

    #[zbus(property)]
    async fn set_auto(&self, value: bool) {
        self.change(Some("Auto"), |model| model.schedule.auto = value).await;
    }

    #[zbus(property)]
    fn day_temperature(&self) -> u16 {
        self.view().day_temperature
    }

    #[zbus(property)]
    async fn set_day_temperature(&self, value: u16) {
        let value = value.clamp(TEMP_MIN, TEMP_MAX);
        self.change(Some("DayTemperature"), |model| model.schedule.day_temperature = value).await;
    }

    #[zbus(property)]
    fn night_temperature(&self) -> u16 {
        self.view().night_temperature
    }

    #[zbus(property)]
    async fn set_night_temperature(&self, value: u16) {
        let value = value.clamp(TEMP_MIN, TEMP_MAX);
        self.change(Some("NightTemperature"), |model| model.schedule.night_temperature = value).await;
    }

    #[zbus(property)]
    fn fallback_day_start(&self) -> u16 {
        self.view().fallback_day_start
    }

    #[zbus(property)]
    async fn set_fallback_day_start(&self, value: u16) -> fdo::Result<()> {
        let value = minutes_of_day(value)?;
        self.change(Some("FallbackDayStart"), |model| model.schedule.fallback_day_start = value).await;
        Ok(())
    }

    #[zbus(property)]
    fn fallback_night_start(&self) -> u16 {
        self.view().fallback_night_start
    }

    #[zbus(property)]
    async fn set_fallback_night_start(&self, value: u16) -> fdo::Result<()> {
        let value = minutes_of_day(value)?;
        self.change(Some("FallbackNightStart"), |model| model.schedule.fallback_night_start = value).await;
        Ok(())
    }

    #[zbus(property)]
    fn has_location(&self) -> bool {
        self.view().has_location
    }

    #[zbus(property)]
    fn latitude(&self) -> f64 {
        self.view().latitude
    }

    #[zbus(property)]
    fn longitude(&self) -> f64 {
        self.view().longitude
    }

    #[zbus(property)]
    fn sunrise(&self) -> u64 {
        self.view().sunrise
    }

    #[zbus(property)]
    fn sunset(&self) -> u64 {
        self.view().sunset
    }

    #[zbus(property)]
    fn sun_elevation(&self) -> f64 {
        self.view().sun_elevation
    }
}

fn changed_colors(old: &Color, new: &Color) -> HashMap<&'static str, Value<'static>> {
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

fn changed_schedule(old: &View, new: &View) -> HashMap<&'static str, Value<'static>> {
    let mut changed = HashMap::new();
    let mut check = |name, moved: bool, value: Value<'static>| {
        if moved {
            changed.insert(name, value);
        }
    };
    check("Auto", old.auto != new.auto, new.auto.into());
    check("DayTemperature", old.day_temperature != new.day_temperature, new.day_temperature.into());
    check("NightTemperature", old.night_temperature != new.night_temperature, new.night_temperature.into());
    check("FallbackDayStart", old.fallback_day_start != new.fallback_day_start, new.fallback_day_start.into());
    check("FallbackNightStart", old.fallback_night_start != new.fallback_night_start, new.fallback_night_start.into());
    check("HasLocation", old.has_location != new.has_location, new.has_location.into());
    check("Latitude", old.latitude != new.latitude, new.latitude.into());
    check("Longitude", old.longitude != new.longitude, new.longitude.into());
    check("Sunrise", old.sunrise != new.sunrise, new.sunrise.into());
    check("Sunset", old.sunset != new.sunset, new.sunset.into());
    check("SunElevation", old.sun_elevation != new.sun_elevation, new.sun_elevation.into());
    changed
}

fn signals(before: &State, after: &State, skip: Skip) -> Vec<Signal> {
    let mut signals = Vec::new();
    // Both snapshots were taken under one lock, so they list the same objects in the same order
    for ((path, old), (_, new)) in before.colors.iter().zip(&after.colors) {
        let mut changed = changed_colors(old, new);
        if let Skip::Color(skipped) = skip
            && skipped == path
        {
            // A setter on one object changes only one property of its own
            changed.clear();
        }
        if !changed.is_empty() {
            signals.push((path.clone(), INTERFACE, changed));
        }
    }
    let mut changed = changed_schedule(&before.schedule, &after.schedule);
    if let Skip::Schedule(name) = skip {
        changed.remove(name);
    }
    if !changed.is_empty() {
        signals.push(("/".to_owned(), SCHEDULE_INTERFACE, changed));
    }
    signals
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(temperatures: &[(&str, u16)], auto: bool) -> State {
        let colors = temperatures.iter().map(|&(p, t)| (p.to_owned(), Color::default().with_temperature(t))).collect();
        let mut model = Model::default();
        model.schedule.auto = auto;
        State { colors, schedule: model.schedule.view(&Now { unix: 0.0, local_minutes: 0.0 }) }
    }

    #[test]
    fn signals_list_only_what_moved() {
        let before = state(&[("/", 6500), ("/outputs/eDP_1", 6500), ("/outputs/DP_1", 6500)], true);
        let after = state(&[("/", 5250), ("/outputs/eDP_1", 4000), ("/outputs/DP_1", 6500)], false);
        let signals = signals(&before, &after, Skip::Nothing);
        let summary: Vec<_> = signals.iter().map(|(p, i, c)| (p.as_str(), *i, c.len())).collect();
        assert_eq!(summary, [("/", INTERFACE, 1), ("/outputs/eDP_1", INTERFACE, 1), ("/", SCHEDULE_INTERFACE, 1)]);
    }

    #[test]
    fn a_setter_skips_its_own_property() {
        let before = state(&[("/", 6500), ("/outputs/eDP_1", 6500)], true);
        let after = state(&[("/", 4000), ("/outputs/eDP_1", 4000)], false);
        let signals = signals(&before, &after, Skip::Color("/outputs/eDP_1"));
        let paths: Vec<_> = signals.iter().map(|(p, i, _)| (p.as_str(), *i)).collect();
        assert_eq!(paths, [("/", INTERFACE), ("/", SCHEDULE_INTERFACE)]);

        let signals = super::signals(&state(&[], true), &state(&[], false), Skip::Schedule("Auto"));
        assert!(signals.is_empty());
    }
}

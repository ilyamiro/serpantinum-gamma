use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use calloop::ping::Ping;
use zbus::zvariant::Value;
use zbus::{Connection, fdo, interface};

use crate::clock::{self, Now};
use crate::color::{Color, TEMP_MAX, TEMP_MIN};
use crate::model::{Model, Target};
use crate::schedule::{Location, MINUTES_PER_DAY, Plan, View};

pub const BUS_NAME: &str = "org.serpantinum.Gamma";
pub const INTERFACE: &str = "org.serpantinum.Gamma";
pub const SCHEDULE_INTERFACE: &str = "org.serpantinum.Gamma.Schedule";
const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";

pub type Shared = Arc<Mutex<Model>>;

/// Locks the model. A panic elsewhere while it was held leaves the model as it was, which is still
/// usable, so a poisoned lock is not worth failing every later call for.
pub fn lock(model: &Shared) -> MutexGuard<'_, Model> {
    model.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Everything the objects show, captured to tell which properties a change moved
pub struct State {
    objects: Vec<(String, Color, Plan)>,
    schedule: View,
}

impl State {
    pub fn capture(model: &Model, now: &Now) -> State {
        State { objects: model.snapshot(), schedule: model.schedule.view(now) }
    }
}

/// The property a setter changed, which zbus signals itself
#[derive(Clone, Copy)]
pub enum Skip<'a> {
    Nothing,
    Color(&'a str),
    Schedule(&'a str, &'static str),
}

/// A `PropertiesChanged` signal to send: the object path, the interface and the changed values
pub type Signal = (String, &'static str, HashMap<&'static str, Value<'static>>);

/// Runs `change` and the schedule on the model, and returns the signals for what moved
fn apply(model: &Shared, skip: Skip, change: impl FnOnce(&mut Model)) -> Vec<Signal> {
    let now = clock::now();
    let mut model = lock(model);
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
        let body = properties_changed(interface, changed);
        let sent = bus.emit_signal(None::<&str>, path.as_str(), PROPERTIES_INTERFACE, "PropertiesChanged", &body);
        report(&path, sent);
    }
}

async fn send(connection: &Connection, signals: Vec<Signal>) {
    for (path, interface, changed) in signals {
        let body = properties_changed(interface, changed);
        let sent =
            connection.emit_signal(None::<&str>, path.as_str(), PROPERTIES_INTERFACE, "PropertiesChanged", &body).await;
        report(&path, sent);
    }
}

fn properties_changed(
    interface: &'static str,
    changed: HashMap<&'static str, Value<'static>>,
) -> (&'static str, HashMap<&'static str, Value<'static>>, Vec<&'static str>) {
    (interface, changed, Vec::new())
}

fn report(path: &str, sent: zbus::Result<()>) {
    if let Err(error) = sent {
        eprintln!("serpantinum-gamma: could not signal a change on {path}: {error}");
    }
}

/// What the two interfaces of one object share: the model, what the object controls, and the way
/// to signal and to wake the Wayland side
#[derive(Clone)]
pub struct Handle {
    model: Shared,
    target: Target,
    path: String,
    connection: Connection,
    wake: Ping,
}

impl Handle {
    pub fn new(model: Shared, target: Target, path: String, connection: Connection, wake: Ping) -> Self {
        Handle { model, target, path, connection, wake }
    }

    /// Applies `change`, wakes the Wayland side to send the new colours, and signals every object
    /// whose values moved except for the property `skip` names, which zbus signals itself
    async fn change(&self, skip: Skip<'_>, change: impl FnOnce(&mut Model)) {
        let signals = apply(&self.model, skip, change);
        self.wake.ping();
        send(&self.connection, signals).await;
    }
}

/// Serves both interfaces at the handle's path
pub fn serve(bus: &zbus::blocking::Connection, handle: Handle) -> zbus::Result<()> {
    let path = handle.path.clone();
    let server = bus.object_server();
    server.at(path.as_str(), Relay { handle: handle.clone() })?;
    server.at(path.as_str(), ScheduleRelay { handle })?;
    Ok(())
}

pub fn unserve(bus: &zbus::blocking::Connection, path: &str) -> zbus::Result<()> {
    let server = bus.object_server();
    server.remove::<Relay, _>(path)?;
    server.remove::<ScheduleRelay, _>(path)?;
    Ok(())
}

/// The `org.serpantinum.Gamma` interface on `/` or on one `/outputs/<name>`
pub struct Relay {
    handle: Handle,
}

impl Relay {
    fn color(&self) -> Color {
        lock(&self.handle.model).color(self.handle.target).unwrap_or_default()
    }

    /// A property setter skips its own property
    async fn change(&self, from_setter: bool, change: impl FnOnce(&mut Model, Target)) {
        let skip = if from_setter { Skip::Color(&self.handle.path) } else { Skip::Nothing };
        let target = self.handle.target;
        self.handle.change(skip, |model| change(model, target)).await;
    }

    /// A temperature set by hand turns the schedule off for the same outputs
    async fn change_temperature(&self, from_setter: bool, change: impl Fn(Color) -> Color) {
        self.change(from_setter, |model, target| {
            model.update_plan(target, |plan| Plan { auto: false, ..plan });
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

    /// Whether the compositor ignores this object's colours, as when another program holds the
    /// control. It is not signalled when it changes.
    #[zbus(property(emits_changed_signal = "false"))]
    fn blocked(&self) -> bool {
        lock(&self.handle.model).blocked(self.handle.target)
    }
}

/// The `org.serpantinum.Gamma.Schedule` interface on `/` or on one `/outputs/<name>`. Auto and the
/// temperatures belong to the object, like the colour; the location and the fallback times are
/// shared by every object.
pub struct ScheduleRelay {
    handle: Handle,
}

impl ScheduleRelay {
    fn plan(&self) -> Plan {
        lock(&self.handle.model).plan(self.handle.target).unwrap_or_default()
    }

    fn view(&self) -> View {
        lock(&self.handle.model).view(&clock::now())
    }

    async fn change(&self, setter: Option<&'static str>, change: impl FnOnce(&mut Model)) {
        let skip = setter.map_or(Skip::Nothing, |name| Skip::Schedule(&self.handle.path, name));
        self.handle.change(skip, change).await;
    }

    async fn change_plan(&self, name: &'static str, change: impl Fn(Plan) -> Plan) {
        let target = self.handle.target;
        self.change(Some(name), |model| model.update_plan(target, change)).await;
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
        self.plan().auto
    }

    #[zbus(property)]
    async fn set_auto(&self, value: bool) {
        self.change_plan("Auto", |plan| Plan { auto: value, ..plan }).await;
    }

    #[zbus(property)]
    fn day_temperature(&self) -> u16 {
        self.plan().day_temperature
    }

    #[zbus(property)]
    async fn set_day_temperature(&self, value: u16) {
        let value = value.clamp(TEMP_MIN, TEMP_MAX);
        self.change_plan("DayTemperature", |plan| Plan { day_temperature: value, ..plan }).await;
    }

    #[zbus(property)]
    fn night_temperature(&self) -> u16 {
        self.plan().night_temperature
    }

    #[zbus(property)]
    async fn set_night_temperature(&self, value: u16) {
        let value = value.clamp(TEMP_MIN, TEMP_MAX);
        self.change_plan("NightTemperature", |plan| Plan { night_temperature: value, ..plan }).await;
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

fn changed_schedule(old: (&Plan, &View), new: (&Plan, &View)) -> HashMap<&'static str, Value<'static>> {
    let mut changed = HashMap::new();
    let mut check = |name, moved: bool, value: Value<'static>| {
        if moved {
            changed.insert(name, value);
        }
    };
    let ((old_plan, old), (new_plan, new)) = (old, new);
    check("Auto", old_plan.auto != new_plan.auto, new_plan.auto.into());
    check("DayTemperature", old_plan.day_temperature != new_plan.day_temperature, new_plan.day_temperature.into());
    check(
        "NightTemperature",
        old_plan.night_temperature != new_plan.night_temperature,
        new_plan.night_temperature.into(),
    );
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
    for ((path, old, old_plan), (_, new, new_plan)) in before.objects.iter().zip(&after.objects) {
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
        let mut changed = changed_schedule((old_plan, &before.schedule), (new_plan, &after.schedule));
        if let Skip::Schedule(skipped, name) = skip
            && skipped == path
        {
            changed.remove(name);
        }
        if !changed.is_empty() {
            signals.push((path.clone(), SCHEDULE_INTERFACE, changed));
        }
    }
    signals
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(objects: &[(&str, u16, bool)]) -> State {
        let objects = objects
            .iter()
            .map(|&(p, t, auto)| (p.to_owned(), Color::default().with_temperature(t), Plan { auto, ..Plan::default() }))
            .collect();
        State { objects, schedule: Model::default().schedule.view(&Now { unix: 0.0, local_minutes: 0.0 }) }
    }

    fn summary(signals: &[Signal]) -> Vec<(&str, &str, usize)> {
        signals.iter().map(|(p, i, c)| (p.as_str(), *i, c.len())).collect()
    }

    #[test]
    fn signals_list_only_what_moved() {
        let before = state(&[("/", 6500, false), ("/outputs/eDP_1", 6500, true), ("/outputs/DP_1", 6500, false)]);
        let after = state(&[("/", 5250, false), ("/outputs/eDP_1", 4000, false), ("/outputs/DP_1", 6500, false)]);
        let signals = signals(&before, &after, Skip::Nothing);
        assert_eq!(
            summary(&signals),
            [("/", INTERFACE, 1), ("/outputs/eDP_1", INTERFACE, 1), ("/outputs/eDP_1", SCHEDULE_INTERFACE, 1)]
        );
    }

    #[test]
    fn a_setter_skips_its_own_property() {
        let before = state(&[("/", 6500, true), ("/outputs/eDP_1", 6500, true)]);
        let after = state(&[("/", 4000, false), ("/outputs/eDP_1", 4000, false)]);
        let signals = signals(&before, &after, Skip::Color("/outputs/eDP_1"));
        let paths: Vec<_> = signals.iter().map(|(p, i, _)| (p.as_str(), *i)).collect();
        assert_eq!(paths, [("/", INTERFACE), ("/", SCHEDULE_INTERFACE), ("/outputs/eDP_1", SCHEDULE_INTERFACE)]);

        let signals = super::signals(
            &before,
            &state(&[("/", 6500, false), ("/outputs/eDP_1", 6500, false)]),
            Skip::Schedule("/", "Auto"),
        );
        assert_eq!(summary(&signals), [("/outputs/eDP_1", SCHEDULE_INTERFACE, 1)]);
    }
}

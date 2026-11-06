use std::collections::BTreeMap;

use crate::clock::Now;
use crate::color::Color;
use crate::schedule::{Plan, Schedule, View};

/// What a D-Bus object controls: `/` (every output) or one `/outputs/<name>`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Root,
    Output(u32),
}

pub struct Output {
    pub name: String,
    pub color: Color,
    pub plan: Plan,
    /// The compositor refused this output's gamma control, so its colours do not take effect
    pub refused: bool,
}

/// The colours asked for over D-Bus, keyed by the outputs' Wayland registry names
#[derive(Default)]
pub struct Model {
    /// The colour a newly connected output starts with; changes made on `/` change it too
    pub default: Color,
    /// The plan a newly connected output starts with, changed along with every output from `/`
    pub default_plan: Plan,
    pub outputs: BTreeMap<u32, Output>,
    pub schedule: Schedule,
    /// The Schedule properties as last signalled, so that changes made by time alone, like the
    /// sun's elevation, are signalled too
    pub signalled: Option<View>,
    /// The compositor ignores every colour sent, as when another program holds Hyprland's colour
    /// matrix control
    pub blocked: bool,
    /// The last [`Model::view`], valid for the second and the schedule it was made for
    view_cache: Option<(i64, Schedule, View)>,
}

impl Model {
    /// For `/`, the average of the outputs (inverted only if all are)
    pub fn color(&self, target: Target) -> Option<Color> {
        match target {
            Target::Output(id) => self.outputs.get(&id).map(|output| output.color),
            Target::Root if self.outputs.is_empty() => Some(self.default),
            Target::Root => Some(Color {
                temperature: self.mean(|o| o.color.temperature as f64) as u16,
                gamma: self.mean(|o| o.color.gamma),
                brightness: self.mean(|o| o.color.brightness),
                inverted: self.outputs.values().all(|o| o.color.inverted),
            }),
        }
    }

    /// For `/`, on only if it is on for every output, with the average temperatures
    pub fn plan(&self, target: Target) -> Option<Plan> {
        match target {
            Target::Output(id) => self.outputs.get(&id).map(|output| output.plan),
            Target::Root if self.outputs.is_empty() => Some(self.default_plan),
            Target::Root => Some(Plan {
                auto: self.outputs.values().all(|o| o.plan.auto),
                day_temperature: self.mean(|o| o.plan.day_temperature as f64) as u16,
                night_temperature: self.mean(|o| o.plan.night_temperature as f64) as u16,
            }),
        }
    }

    /// The average of `value` over the outputs, which must not be empty
    fn mean(&self, value: impl Fn(&Output) -> f64) -> f64 {
        self.outputs.values().map(value).sum::<f64>() / self.outputs.len() as f64
    }

    /// Whether colours for `target` are being ignored by the compositor; for `/`, whether any are
    pub fn blocked(&self, target: Target) -> bool {
        self.blocked
            || match target {
                Target::Output(id) => self.outputs.get(&id).is_some_and(|output| output.refused),
                Target::Root => self.outputs.values().any(|output| output.refused),
            }
    }

    /// The Schedule properties at `now`, recomputed at most once a second: reading several of them
    /// together would otherwise search for the sunrise and sunset each time
    pub fn view(&mut self, now: &Now) -> View {
        let second = now.unix as i64;
        if let Some((cached_second, schedule, view)) = self.view_cache
            && cached_second == second
            && schedule == self.schedule
        {
            return view;
        }
        let view = self.schedule.view(now);
        self.view_cache = Some((second, self.schedule, view));
        view
    }

    fn each(&mut self, target: Target, mut change: impl FnMut(&mut Color, &mut Plan)) {
        match target {
            Target::Root => {
                change(&mut self.default, &mut self.default_plan);
                for output in self.outputs.values_mut() {
                    change(&mut output.color, &mut output.plan);
                }
            }
            Target::Output(id) => {
                if let Some(output) = self.outputs.get_mut(&id) {
                    change(&mut output.color, &mut output.plan);
                }
            }
        }
    }

    pub fn update(&mut self, target: Target, change: impl Fn(Color) -> Color) {
        self.each(target, |color, _| *color = change(*color));
    }

    pub fn update_plan(&mut self, target: Target, change: impl Fn(Plan) -> Plan) {
        self.each(target, |_, plan| *plan = change(*plan));
    }

    /// On `/`: all outputs back to normal if all are inverted, otherwise all inverted
    pub fn toggle_inverted(&mut self, target: Target) {
        let Some(current) = self.color(target) else { return };
        let inverted = !current.inverted;
        self.update(target, |color| Color { inverted, ..color });
    }

    pub fn add_output(&mut self, id: u32, name: String) {
        self.outputs.insert(id, Output { name, color: self.default, plan: self.default_plan, refused: false });
    }

    /// Sets the temperature of every output whose schedule is on to the scheduled one
    pub fn apply_schedule(&mut self, now: &Now) {
        let schedule = self.schedule;
        self.each(Target::Root, |color, plan| {
            if plan.auto {
                *color = color.with_temperature(schedule.temperature(plan, now));
            }
        });
    }

    /// Every object's path with its colour and plan, `/` first
    pub fn snapshot(&self) -> Vec<(String, Color, Plan)> {
        let root =
            ("/".to_owned(), self.color(Target::Root).unwrap_or_default(), self.plan(Target::Root).unwrap_or_default());
        let outputs = self.outputs.values().map(|output| (object_path(&output.name), output.color, output.plan));
        std::iter::once(root).chain(outputs).collect()
    }
}

/// `/outputs/eDP_1` for eDP-1: D-Bus path elements allow only letters, digits and `_`
pub fn object_path(name: &str) -> String {
    let element: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    format!("/outputs/{element}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_outputs() -> Model {
        let mut model = Model::default();
        model.add_output(1, "eDP-1".into());
        model.add_output(2, "HDMI-A-1".into());
        model
    }

    #[test]
    fn root_reports_the_average() {
        let mut model = two_outputs();
        model.update(Target::Output(1), |c| c.with_temperature(4000).with_brightness(0.5));
        let root = model.color(Target::Root).unwrap();
        assert_eq!(root.temperature, 5250);
        assert_eq!(root.brightness, 0.75);
    }

    #[test]
    fn root_changes_every_output_and_the_default() {
        let mut model = two_outputs();
        model.update(Target::Root, |c| c.with_temperature(3000));
        assert!(model.outputs.values().all(|o| o.color.temperature == 3000));
        model.add_output(3, "DP-1".into());
        assert_eq!(model.color(Target::Output(3)).unwrap().temperature, 3000);
    }

    #[test]
    fn root_without_outputs_keeps_what_was_set() {
        let mut model = Model::default();
        model.update(Target::Root, |c| c.with_temperature(2500));
        assert_eq!(model.color(Target::Root).unwrap().temperature, 2500);
    }

    #[test]
    fn deltas_apply_to_each_output_and_clamp() {
        let mut model = two_outputs();
        model.update(Target::Output(1), |c| c.with_brightness(0.95));
        model.update(Target::Root, |c| c.with_brightness(c.brightness + 0.1));
        assert_eq!(model.color(Target::Output(1)).unwrap().brightness, 1.0);
        assert_eq!(model.color(Target::Output(2)).unwrap().brightness, 1.0);
    }

    #[test]
    fn toggling_root_inverts_all_unless_all_are_inverted() {
        let mut model = two_outputs();
        model.toggle_inverted(Target::Output(1));
        model.toggle_inverted(Target::Root);
        assert!(model.outputs.values().all(|o| o.color.inverted));
        model.toggle_inverted(Target::Root);
        assert!(model.outputs.values().all(|o| !o.color.inverted));
    }

    #[test]
    fn schedule_sets_every_output_only_while_on() {
        let mut model = two_outputs();
        let night = Now { unix: 0.0, local_minutes: 0.0 };
        model.apply_schedule(&night);
        assert_eq!(model.color(Target::Root).unwrap().temperature, 6500);
        model.update_plan(Target::Root, |p| Plan { auto: true, ..p });
        model.apply_schedule(&night);
        assert!(model.outputs.values().all(|o| o.color.temperature == 4000));
        model.add_output(3, "DP-1".into());
        assert_eq!(model.color(Target::Output(3)).unwrap().temperature, 4000);
        assert!(model.plan(Target::Output(3)).unwrap().auto);
    }

    #[test]
    fn each_output_follows_its_own_plan() {
        let mut model = two_outputs();
        let night = Now { unix: 0.0, local_minutes: 0.0 };
        model.update_plan(Target::Output(1), |p| Plan { auto: true, night_temperature: 2500, ..p });
        model.update(Target::Output(2), |c| c.with_temperature(5000));
        model.apply_schedule(&night);
        assert_eq!(model.color(Target::Output(1)).unwrap().temperature, 2500);
        assert_eq!(model.color(Target::Output(2)).unwrap().temperature, 5000);
        let root = model.plan(Target::Root).unwrap();
        assert!(!root.auto);
        assert_eq!(root.night_temperature, 3250);
        model.add_output(3, "DP-1".into());
        assert!(!model.plan(Target::Output(3)).unwrap().auto);
    }

    #[test]
    fn schedule_leaves_brightness_gamma_and_inversion_alone() {
        let mut model = two_outputs();
        model.update(Target::Root, |c| Color { inverted: true, ..c.with_brightness(0.6).with_gamma(1.3) });
        model.update_plan(Target::Root, |p| Plan { auto: true, ..p });
        model.apply_schedule(&Now { unix: 0.0, local_minutes: 0.0 });
        let color = model.color(Target::Output(1)).unwrap();
        assert_eq!((color.temperature, color.brightness, color.gamma, color.inverted), (4000, 0.6, 1.3, true));
    }

    #[test]
    fn blocked_follows_the_compositor_and_the_output() {
        let mut model = two_outputs();
        assert!(!model.blocked(Target::Root));
        model.outputs.get_mut(&1).unwrap().refused = true;
        assert!(model.blocked(Target::Output(1)) && model.blocked(Target::Root));
        assert!(!model.blocked(Target::Output(2)));
        model.outputs.get_mut(&1).unwrap().refused = false;
        model.blocked = true;
        assert!(model.blocked(Target::Output(2)));
    }

    #[test]
    fn the_view_is_kept_for_a_second_and_dropped_when_the_schedule_changes() {
        let mut model = Model::default();
        let at = |unix| Now { unix, local_minutes: 0.0 };
        let first = model.view(&at(100.2));
        assert_eq!(model.view(&at(100.9)), first);
        model.schedule.fallback_day_start = 8 * 60;
        assert_eq!(model.view(&at(100.9)).fallback_day_start, 8 * 60);
        model.schedule.location = crate::schedule::Location::new(55.7558, 37.6173);
        let located = model.view(&at(100.9));
        assert!(located.has_location);
        assert_ne!(model.view(&at(101.1)).sun_elevation, located.sun_elevation);
    }

    #[test]
    fn paths_replace_what_d_bus_forbids() {
        assert_eq!(object_path("eDP-1"), "/outputs/eDP_1");
        assert_eq!(object_path("HDMI-A-1"), "/outputs/HDMI_A_1");
        assert_eq!(object_path("Virtual.1"), "/outputs/Virtual_1");
    }
}

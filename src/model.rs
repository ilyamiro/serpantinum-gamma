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
}

impl Model {
    /// For `/`, the average of the outputs (inverted only if all are)
    pub fn color(&self, target: Target) -> Option<Color> {
        match target {
            Target::Output(id) => self.outputs.get(&id).map(|output| output.color),
            Target::Root if self.outputs.is_empty() => Some(self.default),
            Target::Root => {
                let count = self.outputs.len();
                let colors = || self.outputs.values().map(|output| output.color);
                Some(Color {
                    temperature: (colors().map(|c| c.temperature as usize).sum::<usize>() / count) as u16,
                    gamma: colors().map(|c| c.gamma).sum::<f64>() / count as f64,
                    brightness: colors().map(|c| c.brightness).sum::<f64>() / count as f64,
                    inverted: colors().all(|c| c.inverted),
                })
            }
        }
    }

    /// For `/`, on only if it is on for every output, with the average temperatures
    pub fn plan(&self, target: Target) -> Option<Plan> {
        match target {
            Target::Output(id) => self.outputs.get(&id).map(|output| output.plan),
            Target::Root if self.outputs.is_empty() => Some(self.default_plan),
            Target::Root => {
                let count = self.outputs.len();
                let plans = || self.outputs.values().map(|output| output.plan);
                Some(Plan {
                    auto: plans().all(|p| p.auto),
                    day_temperature: (plans().map(|p| p.day_temperature as usize).sum::<usize>() / count) as u16,
                    night_temperature: (plans().map(|p| p.night_temperature as usize).sum::<usize>() / count) as u16,
                })
            }
        }
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
        self.outputs.insert(id, Output { name, color: self.default, plan: self.default_plan });
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
    fn paths_replace_what_d_bus_forbids() {
        assert_eq!(object_path("eDP-1"), "/outputs/eDP_1");
        assert_eq!(object_path("HDMI-A-1"), "/outputs/HDMI_A_1");
        assert_eq!(object_path("Virtual.1"), "/outputs/Virtual_1");
    }
}

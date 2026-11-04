use std::collections::BTreeMap;

use crate::color::Color;

/// What a D-Bus object controls: `/` (every output) or one `/outputs/<name>`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Root,
    Output(u32),
}

pub struct Output {
    pub name: String,
    pub color: Color,
}

/// The colours asked for over D-Bus, keyed by the outputs' Wayland registry names
#[derive(Default)]
pub struct Model {
    /// The colour a newly connected output starts with; changes made on `/` change it too
    pub default: Color,
    pub outputs: BTreeMap<u32, Output>,
}

impl Model {
    /// For `/`, the average of the outputs (inverted only if all are), as wl-gammarelay-rs reports it
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

    pub fn update(&mut self, target: Target, change: impl Fn(Color) -> Color) {
        match target {
            Target::Root => {
                self.default = change(self.default);
                for output in self.outputs.values_mut() {
                    output.color = change(output.color);
                }
            }
            Target::Output(id) => {
                if let Some(output) = self.outputs.get_mut(&id) {
                    output.color = change(output.color);
                }
            }
        }
    }

    /// On `/`: all outputs back to normal if all are inverted, otherwise all inverted
    pub fn toggle_inverted(&mut self, target: Target) {
        let Some(current) = self.color(target) else { return };
        let inverted = !current.inverted;
        self.update(target, |color| Color { inverted, ..color });
    }

    pub fn add_output(&mut self, id: u32, name: String) {
        self.outputs.insert(id, Output { name, color: self.default });
    }

    /// Every object's path with its colour, `/` first
    pub fn snapshot(&self) -> Vec<(String, Color)> {
        let root = ("/".to_owned(), self.color(Target::Root).unwrap_or_default());
        let outputs = self.outputs.values().map(|output| (object_path(&output.name), output.color));
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
    fn paths_replace_what_d_bus_forbids() {
        assert_eq!(object_path("eDP-1"), "/outputs/eDP_1");
        assert_eq!(object_path("HDMI-A-1"), "/outputs/HDMI_A_1");
        assert_eq!(object_path("Virtual.1"), "/outputs/Virtual_1");
    }
}

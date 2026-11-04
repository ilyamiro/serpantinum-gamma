pub const TEMP_MIN: u16 = 1000;
pub const TEMP_MAX: u16 = 10000;
pub const NEUTRAL_TEMP: u16 = 6500;
const GAMMA_MIN: f64 = 0.1;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub temperature: u16,
    pub gamma: f64,
    pub brightness: f64,
    pub inverted: bool,
}

impl Default for Color {
    fn default() -> Self {
        Color { temperature: NEUTRAL_TEMP, gamma: 1.0, brightness: 1.0, inverted: false }
    }
}

impl Color {
    pub fn with_temperature(self, temperature: u16) -> Self {
        Color { temperature: temperature.clamp(TEMP_MIN, TEMP_MAX), ..self }
    }

    pub fn with_brightness(self, brightness: f64) -> Self {
        if !brightness.is_finite() {
            return self;
        }
        Color { brightness: brightness.clamp(0.0, 1.0), ..self }
    }

    pub fn with_gamma(self, gamma: f64) -> Self {
        if !gamma.is_finite() {
            return self;
        }
        Color { gamma: gamma.max(GAMMA_MIN), ..self }
    }
}

// Tanner Helland's fit of the blackbody colour, in 0..=255 per channel
fn helland(kelvin: f64) -> [f64; 3] {
    let t = kelvin / 100.0;
    let red = if t <= 66.0 { 255.0 } else { 329.698727446 * (t - 60.0).powf(-0.1332047592) };
    let green = if t <= 66.0 {
        99.4708025861 * t.ln() - 161.1195681661
    } else {
        288.1221695283 * (t - 60.0).powf(-0.0755148492)
    };
    let blue = if t >= 66.0 {
        255.0
    } else if t <= 19.0 {
        0.0
    } else {
        138.5177312231 * (t - 10.0).ln() - 305.0447927307
    };
    [red.clamp(0.0, 255.0), green.clamp(0.0, 255.0), blue.clamp(0.0, 255.0)]
}

/// Per-channel multipliers in 0..=1, normalised so the neutral temperature is plain white
pub fn white_point(kelvin: u16) -> [f64; 3] {
    let reference = helland(NEUTRAL_TEMP as f64);
    let color = helland(kelvin.clamp(TEMP_MIN, TEMP_MAX) as f64);
    [0, 1, 2].map(|c| (color[c] / reference[c]).min(1.0))
}

/// The ramp the gamma control takes: `size` red values, then green, then blue
pub fn ramp(size: usize, color: Color) -> Vec<u16> {
    let white = white_point(color.temperature);
    let mut values = vec![0u16; size * 3];
    for (channel, multiplier) in white.into_iter().enumerate() {
        for i in 0..size {
            let x = if size > 1 { i as f64 / (size - 1) as f64 } else { 1.0 };
            let value = color.brightness * (x * multiplier).powf(color.gamma);
            let index = if color.inverted { size - 1 - i } else { i };
            values[channel * size + index] = (value.clamp(0.0, 1.0) * 65535.0).round() as u16;
        }
    }
    values
}

pub fn ramp_bytes(size: usize, color: Color) -> Vec<u8> {
    ramp(size, color).into_iter().flat_map(u16::to_ne_bytes).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_ramp_is_linear_for_all_channels() {
        let ramp = ramp(5, Color::default());
        for channel in ramp.chunks(5) {
            assert_eq!(channel, [0, 16384, 32768, 49151, 65535]);
        }
    }

    #[test]
    fn neutral_temperature_is_white() {
        assert_eq!(white_point(NEUTRAL_TEMP), [1.0, 1.0, 1.0]);
    }

    #[test]
    fn warm_temperature_keeps_red_and_cuts_blue() {
        let [red, green, blue] = white_point(3000);
        assert_eq!(red, 1.0);
        assert!(green < 1.0 && blue < green);
    }

    #[test]
    fn brightness_scales_the_top_of_the_ramp() {
        let ramp = ramp(3, Color { brightness: 0.5, ..Color::default() });
        assert_eq!(&ramp[0..3], [0, 16384, 32768]);
    }

    #[test]
    fn gamma_bends_the_middle_only() {
        let ramp = ramp(3, Color { gamma: 2.0, ..Color::default() });
        assert_eq!(&ramp[0..3], [0, 16384, 65535]);
    }

    #[test]
    fn inverted_ramp_runs_backwards() {
        let ramp = ramp(3, Color { inverted: true, ..Color::default() });
        assert_eq!(&ramp[0..3], [65535, 32768, 0]);
    }

    #[test]
    fn setters_clamp_like_wl_gammarelay_rs() {
        let color = Color::default();
        assert_eq!(color.with_temperature(500).temperature, TEMP_MIN);
        assert_eq!(color.with_temperature(20000).temperature, TEMP_MAX);
        assert_eq!(color.with_brightness(1.5).brightness, 1.0);
        assert_eq!(color.with_brightness(-1.0).brightness, 0.0);
        assert_eq!(color.with_gamma(0.0).gamma, GAMMA_MIN);
        assert_eq!(color.with_gamma(f64::NAN), color);
    }
}

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

// The white point is computed from physics rather than a curve fit:
// 1. Planck's law gives the blackbody spectrum at the temperature.
// 2. The CIE 1931 2° colour matching functions turn it into XYZ.
// 3. A Bradford chromatic adaptation maps the 6500 K blackbody onto the display's D65 white, so
//    6500 K changes nothing.
// 4. The XYZ to linear sRGB matrix gives the channels, scaled so the brightest is 1.
// 5. The sRGB transfer function encodes them, since a gamma ramp scales encoded values.

type Vec3 = [f64; 3];
type Mat3 = [Vec3; 3];

const SRGB_FROM_XYZ: Mat3 =
    [[3.2404542, -1.5371385, -0.4985314], [-0.9692660, 1.8760108, 0.0415560], [0.0556434, -0.2040259, 1.0572252]];
const BRADFORD: Mat3 = [[0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]];
const BRADFORD_INVERSE: Mat3 =
    [[0.9869929, -0.1470543, 0.1599627], [0.4323053, 0.5183603, 0.0492912], [-0.0085287, 0.0400428, 0.9684867]];
const D65: Vec3 = [0.95047, 1.0, 1.08883];

fn apply(matrix: &Mat3, v: Vec3) -> Vec3 {
    matrix.map(|row| row[0] * v[0] + row[1] * v[1] + row[2] * v[2])
}

// A piecewise Gaussian lobe, the building block of the CMF fit below
fn lobe(x: f64, mean: f64, below: f64, above: f64) -> f64 {
    let t = (x - mean) / if x < mean { below } else { above };
    (-0.5 * t * t).exp()
}

// CIE 1931 2° colour matching functions, from the multi-lobe fit in Wyman, Sloan and Shirley,
// "Simple Analytic Approximations to the CIE XYZ Color Matching Functions" (JCGT, 2013)
fn color_matching(nm: f64) -> Vec3 {
    [
        1.056 * lobe(nm, 599.8, 37.9, 31.0) + 0.362 * lobe(nm, 442.0, 16.0, 26.7) - 0.065 * lobe(nm, 501.1, 20.4, 26.2),
        0.821 * lobe(nm, 568.8, 46.9, 40.5) + 0.286 * lobe(nm, 530.9, 16.3, 31.1),
        1.217 * lobe(nm, 437.0, 11.8, 36.0) + 0.681 * lobe(nm, 459.0, 26.0, 13.8),
    ]
}

// Blackbody XYZ by Planck's law over the visible range, scaled to Y = 1
fn blackbody_xyz(kelvin: f64) -> Vec3 {
    const SECOND_RADIATION_CONSTANT: f64 = 1.438_776_877e-2; // h·c/k in m·K
    let mut xyz = [0.0; 3];
    for nm in 380..=780 {
        let metres = nm as f64 * 1e-9;
        let radiance = 1.0 / (metres.powi(5) * ((SECOND_RADIATION_CONSTANT / (metres * kelvin)).exp() - 1.0));
        let cmf = color_matching(nm as f64);
        for c in 0..3 {
            xyz[c] += radiance * cmf[c];
        }
    }
    xyz.map(|v| v / xyz[1])
}

fn srgb_encode(linear: f64) -> f64 {
    if linear <= 0.0031308 { 12.92 * linear } else { 1.055 * linear.powf(1.0 / 2.4) - 0.055 }
}

/// Per-channel multipliers in 0..=1 for the encoded signal; the neutral temperature is plain white
pub fn white_point(kelvin: u16) -> [f64; 3] {
    linear_white_point(kelvin).map(|v| if v >= 1.0 { 1.0 } else { srgb_encode(v) })
}

/// The same multipliers on linear light, which is what a colour matrix scales
fn linear_white_point(kelvin: u16) -> [f64; 3] {
    let kelvin = kelvin.clamp(TEMP_MIN, TEMP_MAX);
    if kelvin == NEUTRAL_TEMP {
        return [1.0; 3];
    }
    let source = apply(&BRADFORD, blackbody_xyz(NEUTRAL_TEMP as f64));
    let target = apply(&BRADFORD, D65);
    let cone = apply(&BRADFORD, blackbody_xyz(kelvin as f64));
    let adapted = apply(&BRADFORD_INVERSE, [0, 1, 2].map(|c| cone[c] * target[c] / source[c]));
    let linear = apply(&SRGB_FROM_XYZ, adapted).map(|v| v.max(0.0));
    let peak = linear.into_iter().fold(0.0, f64::max);
    linear.map(|v| v / peak)
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

/// The row-major 3x3 colour matrix of Hyprland's CTM control. A matrix cannot bend gamma or
/// invert, since it is linear and its entries may not be negative, so only temperature and
/// brightness are carried.
pub fn ctm(color: Color) -> [f64; 9] {
    let [r, g, b] = linear_white_point(color.temperature).map(|v| v * color.brightness);
    [r, 0.0, 0.0, 0.0, g, 0.0, 0.0, 0.0, b]
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

    fn assert_close(actual: [f64; 3], expected: [f64; 3]) {
        for (a, e) in actual.into_iter().zip(expected) {
            assert!((a - e).abs() < 1e-3, "{actual:?} is not {expected:?}");
        }
    }

    // Reference values from an independent Python implementation of the same steps
    #[test]
    fn white_points_match_the_reference() {
        assert_close(white_point(1000), [1.0, 0.1926, 0.0]);
        assert_close(white_point(1900), [1.0, 0.5393, 0.0]);
        assert_close(white_point(3000), [1.0, 0.7392, 0.4262]);
        assert_close(white_point(4500), [1.0, 0.8911, 0.7372]);
        assert_close(white_point(10000), [0.7989, 0.8704, 1.0]);
    }

    #[test]
    fn white_point_is_continuous_around_neutral() {
        assert_close(white_point(NEUTRAL_TEMP - 1), [1.0; 3]);
        assert_close(white_point(NEUTRAL_TEMP + 1), [1.0; 3]);
    }

    #[test]
    fn warm_temperature_keeps_red_and_cuts_blue() {
        let [red, green, blue] = white_point(3000);
        assert!((red - 1.0).abs() < 1e-9);
        assert!(green < 1.0 && blue < green);
    }

    #[test]
    fn ctm_is_diagonal_and_scales_linear_light() {
        assert_eq!(ctm(Color::default()), [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
        let warm = ctm(Color { temperature: 3000, brightness: 0.5, ..Color::default() });
        assert!((warm[0] - 0.5).abs() < 1e-9);
        assert!(warm[4] < 0.5 && warm[8] < warm[4]);
        assert!(warm.iter().all(|v| *v >= 0.0));
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
    fn setters_clamp_to_the_valid_ranges() {
        let color = Color::default();
        assert_eq!(color.with_temperature(500).temperature, TEMP_MIN);
        assert_eq!(color.with_temperature(20000).temperature, TEMP_MAX);
        assert_eq!(color.with_brightness(1.5).brightness, 1.0);
        assert_eq!(color.with_brightness(-1.0).brightness, 0.0);
        assert_eq!(color.with_gamma(0.0).gamma, GAMMA_MIN);
        assert_eq!(color.with_gamma(f64::NAN), color);
    }
}

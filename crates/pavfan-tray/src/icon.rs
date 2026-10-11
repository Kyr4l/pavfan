// SPDX-License-Identifier: GPL-3.0-or-later
//! Tray icon: theme icon names (shipped as SVGs in `packaging/icons/`) plus a
//! procedurally drawn ARGB fallback for hosts that cannot find them.
//! The pixmap mirrors the SVG: grey ring, three blades in the state colour, hub.

use ksni::Icon;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconState {
    /// Daemon unreachable or unhealthy.
    Error,
    /// Mode OFF: the BIOS owns the fans.
    Off,
    /// pavfan active but currently deferring to the BIOS (AUTO below the threshold).
    Idle,
    /// pavfan is driving the fans.
    Active,
    /// Hard thermal override: maximum fans.
    Hot,
}

impl IconState {
    pub fn name(self) -> &'static str {
        match self {
            IconState::Error => "pavfan-error",
            IconState::Off => "pavfan-off",
            IconState::Idle => "pavfan-idle",
            IconState::Active => "pavfan-active",
            IconState::Hot => "pavfan-hot",
        }
    }

    /// Blade colour (R, G, B); ring and hub are neutral grey so they read on light and dark panels.
    fn rgb(self) -> (u8, u8, u8) {
        match self {
            IconState::Error => (0xDA, 0x44, 0x53),
            IconState::Off => (0x80, 0x80, 0x80),
            IconState::Idle => (0x3D, 0xAE, 0xE9),
            IconState::Active => (0x27, 0xAE, 0x60),
            IconState::Hot => (0xF6, 0x74, 0x00),
        }
    }
}

const RING: (u8, u8, u8) = (0x80, 0x80, 0x80);
/// Blades: 3 sectors of 70 degrees, starting at -90, 30, 150 (matches the SVGs).
const BLADE_STARTS_DEG: [f32; 3] = [-90.0, 30.0, 150.0];
const BLADE_WIDTH_DEG: f32 = 70.0;

pub fn pixmaps(state: IconState) -> Vec<Icon> {
    [22, 32, 48].into_iter().map(|s| render(s, state)).collect()
}

/// Render one square icon; 3x3 supersampling for smooth edges.
fn render(size: i32, state: IconState) -> Icon {
    let n = size.max(1);
    let nf = n as f32;
    let centre = nf / 2.0;
    let radius = nf / 2.0;
    let blade = state.rgb();
    let mut data = Vec::with_capacity((n * n * 4) as usize);

    for py in 0..n {
        for px in 0..n {
            let (mut a, mut r, mut g, mut b) = (0u32, 0u32, 0u32, 0u32);
            for sy in 0..3 {
                for sx in 0..3 {
                    let x = px as f32 + (sx as f32 + 0.5) / 3.0 - centre;
                    let y = py as f32 + (sy as f32 + 0.5) / 3.0 - centre;
                    if let Some((cr, cg, cb)) = sample(x / radius, y / radius, blade, state) {
                        a += 255;
                        r += u32::from(cr);
                        g += u32::from(cg);
                        b += u32::from(cb);
                    }
                }
            }
            // Average over the 9 samples; colour channels are averaged over covered samples only.
            let covered = a / 255;
            let alpha = (a / 9).min(255) as u8;
            let avg = |v: u32| v.checked_div(covered).unwrap_or(0) as u8;
            let (r, g, b) = (avg(r), avg(g), avg(b));
            // SNI pixmaps are ARGB32 in network byte order.
            data.extend_from_slice(&[alpha, r, g, b]);
        }
    }
    Icon { width: n, height: n, data }
}

/// Colour at a point in unit coordinates (origin at the centre, radius 1), or None if transparent.
fn sample(x: f32, y: f32, blade: (u8, u8, u8), state: IconState) -> Option<(u8, u8, u8)> {
    let r = (x * x + y * y).sqrt();
    if r > 0.96 {
        return None;
    }
    if r >= 0.82 {
        return Some(RING); // outer ring, ~0.14 wide
    }
    if state == IconState::Error {
        // A cross instead of blades: unmistakable even at 16 px.
        let (ax, ay) = (x.abs(), y.abs());
        if (ax - ay).abs() < 0.13 && ax < 0.6 {
            return Some(blade);
        }
        return None;
    }
    if r < 0.18 {
        return Some(RING); // hub
    }
    if r < 0.71 {
        let angle = y.atan2(x).to_degrees(); // -180..180, clockwise in screen coordinates
        for start in BLADE_STARTS_DEG {
            let rel = (angle - start).rem_euclid(360.0);
            if rel <= BLADE_WIDTH_DEG {
                return Some(blade);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixmap_has_expected_size_and_opaque_ring() {
        let icon = render(22, IconState::Active);
        assert_eq!(icon.data.len(), 22 * 22 * 4);
        // the centre pixel is the hub (opaque grey); the corner is transparent
        let centre = (11 * 22 + 11) * 4;
        assert_eq!(icon.data.get(centre), Some(&255));
        assert_eq!(icon.data.first(), Some(&0));
    }

    #[test]
    fn blades_cover_the_top_sector() {
        // straight up (-90 deg) at mid radius is inside the first blade
        assert_eq!(sample(0.0, -0.5, (1, 2, 3), IconState::Active), Some((1, 2, 3)));
        // 10 degrees before the first blade start is a gap
        assert_eq!(sample(-0.087, -0.49, (1, 2, 3), IconState::Active), None);
    }
}

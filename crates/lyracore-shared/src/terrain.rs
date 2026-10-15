use crate::spatial::MAP_COORD_MAX;

// MCNK cell side length in yards.
pub const CELL_SIZE: f32 = 533.333_3 / 16.0;
// Nine outer corners per axis span eight terrain quads.
pub const QUAD: f32 = CELL_SIZE / 8.0;

// Comparison chains preserve legacy NaN acceptance; Range::contains would refuse it.
#[allow(clippy::manual_range_contains)]
pub fn cell_index(coord: f32) -> Option<u16> {
    let c = (MAP_COORD_MAX - coord) / CELL_SIZE;
    if c < 0.0 || c >= 1024.0 {
        return None;
    }
    Some(c as u16)
}

pub fn cell_key(map_id: u32, cell_x: u16, cell_y: u16) -> u64 {
    ((map_id as u64) << 32) | ((cell_x as u64) << 16) | cell_y as u64
}

// MCVT has 145 floats: nine outer and eight inner heights per row pair.
pub fn interpolate(heights: &[f32], cell_x: u16, cell_y: u16, x: f32, y: f32) -> Option<f32> {
    if heights.len() != 145 {
        return None;
    }
    if cell_index(x) != Some(cell_x) || cell_index(y) != Some(cell_y) {
        return None;
    }
    let lx = ((MAP_COORD_MAX - x) - cell_x as f32 * CELL_SIZE) / QUAD;
    let ly = ((MAP_COORD_MAX - y) - cell_y as f32 * CELL_SIZE) / QUAD;
    let (ix, iy) = ((lx as usize).min(7), (ly as usize).min(7));
    let (fx, fy) = (lx - ix as f32, ly - iy as f32);
    let corner = |i: usize, j: usize| heights[i * 17 + j];
    let top = corner(ix, iy) * (1.0 - fy) + corner(ix, iy + 1) * fy;
    let bot = corner(ix + 1, iy) * (1.0 - fy) + corner(ix + 1, iy + 1) * fy;
    Some(top * (1.0 - fx) + bot * fx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bilinear_recovers_a_sloped_plane() {
        let mut h = vec![0.0f32; 145];
        for i in 0..9 {
            for j in 0..9 {
                h[i * 17 + j] = i as f32;
            }
        }
        let (x0, y0) = (-8949.95f32, -132.493f32);
        let (cx, cy) = (cell_index(x0).unwrap(), cell_index(y0).unwrap());
        let corner_x = MAP_COORD_MAX - cx as f32 * CELL_SIZE;
        let corner_y = MAP_COORD_MAX - cy as f32 * CELL_SIZE;
        let z = interpolate(
            &h,
            cx,
            cy,
            corner_x - CELL_SIZE / 2.0,
            corner_y - CELL_SIZE / 4.0,
        );
        assert!((z.unwrap() - 4.0).abs() < 1e-3);
    }

    #[test]
    fn flat_cell_is_exact() {
        let h = vec![83.5312f32; 145];
        let (x0, y0) = (-8949.95f32, -132.493f32);
        let (cx, cy) = (cell_index(x0).unwrap(), cell_index(y0).unwrap());
        let z = interpolate(&h, cx, cy, x0, y0).unwrap();
        assert!((z - 83.5312).abs() < 1e-4);
    }

    #[test]
    fn rejects_wrong_cell_and_bad_array() {
        let h = vec![10.0f32; 145];
        assert!(interpolate(&h, 0, 0, -8949.95, -132.493).is_none());
        assert!(interpolate(&vec![10.0f32; 144], 779, 516, -8905.0, -153.0).is_none());
        assert!(cell_index(20000.0).is_none());
    }
}

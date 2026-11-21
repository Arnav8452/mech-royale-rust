use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawWorld {
    bounds: RawBounds,
    terrain: RawTerrain,
    prisms: Vec<RawPrism>,
    #[serde(default)]
    rules: RawRules,
    #[serde(default)]
    trees: RawTrees,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawRules {
    canopy_density: Option<f64>,
    canopy_block_t: Option<f64>,
}

#[derive(Deserialize, Default)]
struct RawTrees {
    #[serde(default)]
    fields: Vec<String>,
    #[serde(default)]
    data: Vec<f64>,
}

#[derive(Debug, Clone)]
pub struct Tree {
    pub crown_x: f64,
    pub crown_y: f64,
    pub crown_base_z: f64,
    pub crown_top_z: f64,
    pub crown_r: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawBounds {
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTerrain {
    file: String,
    min_x: f64,
    min_y: f64,
    step: f64,
    nx: usize,
    ny: usize,
    scale: f64,
}

#[derive(Deserialize)]
struct RawPrism {
    #[serde(default)]
    #[allow(dead_code)]
    kind: String,
    base: f64,
    top: f64,
    ring: Vec<f64>,
    #[serde(default)]
    holes: Vec<Vec<f64>>,
}

#[derive(Debug, Clone)]
pub struct Prism {
    pub min_x: f64,
    pub max_x: f64,
    pub min_y: f64,
    pub max_y: f64,
    pub base: f64,
    pub top: f64,
    pub ring: Vec<[f64; 2]>,
    pub holes: Vec<Vec<[f64; 2]>>,
}

pub struct Map {
    pub min_x: f64,
    pub max_x: f64,
    pub min_y: f64,
    pub max_y: f64,
    t_min_x: f64,
    t_min_y: f64,
    t_step: f64,
    t_nx: usize,
    t_ny: usize,
    heights: Vec<f32>,
    pub prisms: Vec<Prism>,
    pub trees: Vec<Tree>,
    pub canopy_block_m: f64,
    grid_cell: f64,
    grid_nx: usize,
    grid_ny: usize,
    grid_origin_x: f64,
    grid_origin_y: f64,
    grid: Vec<Vec<usize>>,
    tree_grid: Vec<Vec<usize>>,
}

impl Map {
    pub fn load(map_dir: &str, files: &[String]) -> Result<Self, String> {
        let base_dir = Path::new(map_dir);
        let world_rel = files
            .iter()
            .find(|f| f.ends_with(".json"))
            .map(|s| s.as_str())
            .unwrap_or("v2/world.json");
        let world_path = base_dir.join(world_rel);
        let world_bytes = fs::read(&world_path)
            .map_err(|e| format!("failed to read {}: {e}", world_path.display()))?;
        let raw: RawWorld = serde_json::from_slice(&world_bytes)
            .map_err(|e| format!("failed to parse {}: {e}", world_path.display()))?;

        let terrain_path = resolve_terrain_path(base_dir, &world_path, &raw.terrain.file, files);
        let terrain_bytes = fs::read(&terrain_path)
            .map_err(|e| format!("failed to read {}: {e}", terrain_path.display()))?;

        let expected = raw.terrain.nx * raw.terrain.ny;
        if terrain_bytes.len() < expected * 2 {
            return Err(format!(
                "terrain file {} too small: {} < {}",
                terrain_path.display(),
                terrain_bytes.len(),
                expected * 2
            ));
        }

        let scale = raw.terrain.scale as f32;
        let mut heights = Vec::with_capacity(expected);
        for chunk in terrain_bytes[..expected * 2].chunks_exact(2) {
            let v = i16::from_le_bytes([chunk[0], chunk[1]]);
            heights.push(v as f32 * scale);
        }

        let mut prisms = Vec::with_capacity(raw.prisms.len());
        for rp in raw.prisms {
            let ring = pairs_to_pts(&rp.ring);
            if ring.len() < 3 {
                continue;
            }
            let mut min_x = f64::INFINITY;
            let mut max_x = f64::NEG_INFINITY;
            let mut min_y = f64::INFINITY;
            let mut max_y = f64::NEG_INFINITY;
            for &[x, y] in &ring {
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
            }
            let holes = rp
                .holes
                .into_iter()
                .map(|h| pairs_to_pts(&h))
                .filter(|h| h.len() >= 3)
                .collect();
            prisms.push(Prism {
                min_x,
                max_x,
                min_y,
                max_y,
                base: rp.base,
                top: rp.top,
                ring,
                holes,
            });
        }

        let grid_cell = 16.0;
        let grid_origin_x = raw.bounds.min_x - 32.0;
        let grid_origin_y = raw.bounds.min_y - 32.0;
        let span_x = (raw.bounds.max_x + 32.0) - grid_origin_x;
        let span_y = (raw.bounds.max_y + 32.0) - grid_origin_y;
        let grid_nx = ((span_x / grid_cell).ceil() as usize).max(1);
        let grid_ny = ((span_y / grid_cell).ceil() as usize).max(1);
        let mut grid = vec![Vec::new(); grid_nx * grid_ny];

        for (idx, p) in prisms.iter().enumerate() {
            let cx0 = (((p.min_x - grid_origin_x) / grid_cell).floor() as isize)
                .clamp(0, (grid_nx - 1) as isize) as usize;
            let cx1 = (((p.max_x - grid_origin_x) / grid_cell).floor() as isize)
                .clamp(0, (grid_nx - 1) as isize) as usize;
            let cy0 = (((p.min_y - grid_origin_y) / grid_cell).floor() as isize)
                .clamp(0, (grid_ny - 1) as isize) as usize;
            let cy1 = (((p.max_y - grid_origin_y) / grid_cell).floor() as isize)
                .clamp(0, (grid_ny - 1) as isize) as usize;
            for cy in cy0..=cy1 {
                for cx in cx0..=cx1 {
                    grid[cy * grid_nx + cx].push(idx);
                }
            }
        }

        let canopy_density = raw.rules.canopy_density.unwrap_or(0.35).max(0.05);
        let canopy_block_t = raw.rules.canopy_block_t.unwrap_or(0.25).clamp(0.01, 0.99);
        let canopy_block_m = (-canopy_block_t.ln() / canopy_density).clamp(2.0, 8.0);

        let mut trees = Vec::new();
        let mut tree_grid = vec![Vec::new(); grid_nx * grid_ny];
        let nf = raw.trees.fields.len();
        if nf > 0 && !raw.trees.data.is_empty() {
            let fidx = |name: &str| raw.trees.fields.iter().position(|f| f == name);
            if let (Some(iz), Some(ih), Some(icb), Some(icr)) = (
                fidx("z"),
                fidx("height"),
                fidx("crownBase"),
                fidx("crownR"),
            ) {
                let icx = fidx("crownX").or_else(|| fidx("x"));
                let icy = fidx("crownY").or_else(|| fidx("y"));
                if let (Some(icx), Some(icy)) = (icx, icy) {
                    for row in raw.trees.data.chunks_exact(nf) {
                        let crown_x = row[icx];
                        let crown_y = row[icy];
                        let z = row[iz];
                        let crown_r = row[icr];
                        let crown_base_z = z + row[icb];
                        let crown_top_z = z + row[ih];
                        if crown_r > 0.0
                            && crown_top_z > crown_base_z
                            && crown_x.is_finite()
                            && crown_y.is_finite()
                        {
                            let tidx = trees.len();
                            let cx0 = (((crown_x - crown_r - grid_origin_x) / grid_cell).floor()
                                as isize)
                                .clamp(0, (grid_nx - 1) as isize)
                                as usize;
                            let cx1 = (((crown_x + crown_r - grid_origin_x) / grid_cell).floor()
                                as isize)
                                .clamp(0, (grid_nx - 1) as isize)
                                as usize;
                            let cy0 = (((crown_y - crown_r - grid_origin_y) / grid_cell).floor()
                                as isize)
                                .clamp(0, (grid_ny - 1) as isize)
                                as usize;
                            let cy1 = (((crown_y + crown_r - grid_origin_y) / grid_cell).floor()
                                as isize)
                                .clamp(0, (grid_ny - 1) as isize)
                                as usize;
                            for cy in cy0..=cy1 {
                                for cx in cx0..=cx1 {
                                    tree_grid[cy * grid_nx + cx].push(tidx);
                                }
                            }
                            trees.push(Tree {
                                crown_x,
                                crown_y,
                                crown_base_z,
                                crown_top_z,
                                crown_r,
                            });
                        }
                    }
                }
            }
        }

        Ok(Self {
            min_x: raw.bounds.min_x,
            max_x: raw.bounds.max_x,
            min_y: raw.bounds.min_y,
            max_y: raw.bounds.max_y,
            t_min_x: raw.terrain.min_x,
            t_min_y: raw.terrain.min_y,
            t_step: raw.terrain.step,
            t_nx: raw.terrain.nx,
            t_ny: raw.terrain.ny,
            heights,
            prisms,
            trees,
            canopy_block_m,
            grid_cell,
            grid_nx,
            grid_ny,
            grid_origin_x,
            grid_origin_y,
            grid,
            tree_grid,
        })
    }

    pub fn fallback_empty() -> Self {
        Self {
            min_x: -600.0,
            max_x: 600.0,
            min_y: -600.0,
            max_y: 600.0,
            t_min_x: -600.0,
            t_min_y: -600.0,
            t_step: 600.0,
            t_nx: 3,
            t_ny: 3,
            heights: vec![0.0; 9],
            prisms: Vec::new(),
            trees: Vec::new(),
            canopy_block_m: 3.96,
            grid_cell: 64.0,
            grid_nx: 1,
            grid_ny: 1,
            grid_origin_x: -600.0,
            grid_origin_y: -600.0,
            grid: vec![Vec::new()],
            tree_grid: vec![Vec::new()],
        }
    }

    #[inline]
    pub fn ground_height(&self, x: f64, y: f64) -> f64 {
        if self.t_nx < 2 || self.t_ny < 2 {
            return 0.0;
        }
        let fx = ((x - self.t_min_x) / self.t_step).clamp(0.0, (self.t_nx - 1) as f64);
        let fy = ((y - self.t_min_y) / self.t_step).clamp(0.0, (self.t_ny - 1) as f64);
        let i0 = (fx.floor() as usize).min(self.t_nx - 2);
        let j0 = (fy.floor() as usize).min(self.t_ny - 2);
        let tx = fx - i0 as f64;
        let ty = fy - j0 as f64;

        let idx00 = j0 * self.t_nx + i0;
        let h00 = self.heights[idx00] as f64;
        let h10 = self.heights[idx00 + 1] as f64;
        let h01 = self.heights[idx00 + self.t_nx] as f64;
        let h11 = self.heights[idx00 + self.t_nx + 1] as f64;

        let hx0 = h00 + (h10 - h00) * tx;
        let hx1 = h01 + (h11 - h01) * tx;
        hx0 + (hx1 - hx0) * ty
    }

    pub fn point_in_prism_2d(&self, prism_idx: usize, x: f64, y: f64) -> bool {
        let p = &self.prisms[prism_idx];
        if x < p.min_x || x > p.max_x || y < p.min_y || y > p.max_y {
            return false;
        }
        if !point_in_ring(&p.ring, x, y) {
            return false;
        }
        for hole in &p.holes {
            if point_in_ring(hole, x, y) {
                return false;
            }
        }
        true
    }

    pub fn dist_sq_to_prism_2d(&self, prism_idx: usize, x: f64, y: f64) -> f64 {
        let p = &self.prisms[prism_idx];
        let dx_box = if x < p.min_x {
            p.min_x - x
        } else if x > p.max_x {
            x - p.max_x
        } else {
            0.0
        };
        let dy_box = if y < p.min_y {
            p.min_y - y
        } else if y > p.max_y {
            y - p.max_y
        } else {
            0.0
        };
        if dx_box > 0.0 || dy_box > 0.0 {
            return min_dist_sq_to_ring(&p.ring, x, y);
        }
        if self.point_in_prism_2d(prism_idx, x, y) {
            return 0.0;
        }
        let mut best = min_dist_sq_to_ring(&p.ring, x, y);
        if point_in_ring(&p.ring, x, y) {
            for hole in &p.holes {
                best = best.min(min_dist_sq_to_ring(hole, x, y));
            }
        }
        best
    }

    pub fn prism_within_dist_sq(&self, prism_idx: usize, x: f64, y: f64, max_d2: f64) -> bool {
        let p = &self.prisms[prism_idx];
        let dx_box = if x < p.min_x {
            p.min_x - x
        } else if x > p.max_x {
            x - p.max_x
        } else {
            0.0
        };
        let dy_box = if y < p.min_y {
            p.min_y - y
        } else if y > p.max_y {
            y - p.max_y
        } else {
            0.0
        };
        if dx_box * dx_box + dy_box * dy_box >= max_d2 {
            return false;
        }
        self.dist_sq_to_prism_2d(prism_idx, x, y) < max_d2
    }

    fn for_each_prism_in_aabb<F>(&self, min_x: f64, max_x: f64, min_y: f64, max_y: f64, mut f: F)
    where
        F: FnMut(usize) -> bool,
    {
        let cx0 = (((min_x - self.grid_origin_x) / self.grid_cell).floor() as isize)
            .clamp(0, (self.grid_nx - 1) as isize) as usize;
        let cx1 = (((max_x - self.grid_origin_x) / self.grid_cell).floor() as isize)
            .clamp(0, (self.grid_nx - 1) as isize) as usize;
        let cy0 = (((min_y - self.grid_origin_y) / self.grid_cell).floor() as isize)
            .clamp(0, (self.grid_ny - 1) as isize) as usize;
        let cy1 = (((max_y - self.grid_origin_y) / self.grid_cell).floor() as isize)
            .clamp(0, (self.grid_ny - 1) as isize) as usize;

        let mut seen = [usize::MAX; 32];
        let mut seen_len = 0usize;

        for cy in cy0..=cy1 {
            for cx in cx0..=cx1 {
                for &idx in &self.grid[cy * self.grid_nx + cx] {
                    if seen[..seen_len].contains(&idx) {
                        continue;
                    }
                    if seen_len < seen.len() {
                        seen[seen_len] = idx;
                        seen_len += 1;
                    }
                    let p = &self.prisms[idx];
                    if max_x < p.min_x || min_x > p.max_x || max_y < p.min_y || min_y > p.max_y {
                        continue;
                    }
                    if !f(idx) {
                        return;
                    }
                }
            }
        }
    }

    pub fn surface_height(&self, x: f64, y: f64, ref_z: f64) -> f64 {
        let mut h = self.ground_height(x, y);
        self.for_each_prism_in_aabb(x, x, y, y, |idx| {
            let p = &self.prisms[idx];
            if p.top <= ref_z + 1.8 && self.point_in_prism_2d(idx, x, y) {
                h = h.max(p.top);
            }
            true
        });
        h
    }

    pub fn solid_top_at(&self, x: f64, y: f64, ray_ref_z: f64) -> f64 {
        let mut h = self.ground_height(x, y);
        self.for_each_prism_in_aabb(x, x, y, y, |idx| {
            let p = &self.prisms[idx];
            if p.base <= ray_ref_z && self.point_in_prism_2d(idx, x, y) {
                h = h.max(p.top);
            }
            true
        });
        h
    }

    pub fn point_in_canopy(&self, x: f64, y: f64, z: f64) -> bool {
        if self.trees.is_empty() {
            return false;
        }
        let cx = (((x - self.grid_origin_x) / self.grid_cell).floor() as isize)
            .clamp(0, (self.grid_nx - 1) as isize) as usize;
        let cy = (((y - self.grid_origin_y) / self.grid_cell).floor() as isize)
            .clamp(0, (self.grid_ny - 1) as isize) as usize;
        for &tidx in &self.tree_grid[cy * self.grid_nx + cx] {
            let tr = &self.trees[tidx];
            if z >= tr.crown_base_z && z <= tr.crown_top_z {
                let dx = x - tr.crown_x;
                let dy = y - tr.crown_y;
                if dx * dx + dy * dy <= tr.crown_r * tr.crown_r {
                    return true;
                }
            }
        }
        false
    }

    pub fn has_tall_prism_near(&self, x: f64, y: f64, ground_z: f64, probe_r: f64) -> bool {
        let mut found = false;
        let r2 = probe_r * probe_r;
        self.for_each_prism_in_aabb(x - probe_r, x + probe_r, y - probe_r, y + probe_r, |idx| {
            let p = &self.prisms[idx];
            if p.top >= ground_z + 6.0
                && p.base <= ground_z + 3.0
                && self.dist_sq_to_prism_2d(idx, x, y) <= r2
            {
                found = true;
                return false;
            }
            true
        });
        found
    }

    pub fn is_pos_walkable(
        &self,
        x: f64,
        y: f64,
        feet_z: f64,
        radius: f64,
        height: f64,
        step_up: f64,
    ) -> (bool, f64) {
        if x < self.min_x + radius
            || x > self.max_x - radius
            || y < self.min_y + radius
            || y > self.max_y - radius
        {
            return (false, feet_z);
        }

        let mut surface_z = self.ground_height(x, y);
        let r2 = radius * radius;
        let mut blocked = false;

        self.for_each_prism_in_aabb(x - radius, x + radius, y - radius, y + radius, |idx| {
            let p = &self.prisms[idx];
            if p.base < feet_z + height && p.top > feet_z + step_up {
                if self.prism_within_dist_sq(idx, x, y, r2) {
                    blocked = true;
                    return false;
                }
            } else if p.top <= feet_z + step_up && self.point_in_prism_2d(idx, x, y) {
                surface_z = surface_z.max(p.top);
            }
            true
        });

        (!blocked, surface_z)
    }

    pub fn has_los(&self, a: [f64; 3], b: [f64; 3]) -> bool {
        let dx = b[0] - a[0];
        let dy = b[1] - a[1];
        let dz = b[2] - a[2];
        let len_xy = dx.hypot(dy);
        let len_3d = (dx * dx + dy * dy + dz * dz).sqrt();
        if len_3d < 1e-4 {
            return true;
        }

        // Sample terrain along the ray at <= 3 m intervals
        let steps = ((len_xy / 3.0).ceil() as usize).clamp(1, 120);
        for s in 1..steps {
            let t = (s as f64) / (steps as f64);
            let px = a[0] + dx * t;
            let py = a[1] + dy * t;
            let pz = a[2] + dz * t;
            if pz < self.ground_height(px, py) - 0.12 {
                return false;
            }
        }

        // Collect candidate prisms using 2D grid DDA from a.xy to b.xy
        let candidates = self.collect_segment_prisms(a[0], a[1], b[0], b[1]);
        let seg_min_x = a[0].min(b[0]);
        let seg_max_x = a[0].max(b[0]);
        let seg_min_y = a[1].min(b[1]);
        let seg_max_y = a[1].max(b[1]);
        let seg_min_z = a[2].min(b[2]);
        let seg_max_z = a[2].max(b[2]);

        for idx in candidates {
            let p = &self.prisms[idx];
            if p.max_x < seg_min_x
                || p.min_x > seg_max_x
                || p.max_y < seg_min_y
                || p.min_y > seg_max_y
                || p.top < seg_min_z
                || p.base > seg_max_z
            {
                continue;
            }

            let (mut t0, mut t1) = if dz.abs() < 1e-9 {
                if a[2] < p.base || a[2] > p.top {
                    continue;
                }
                (0.0_f64, 1.0_f64)
            } else {
                let tz0 = (p.base - a[2]) / dz;
                let tz1 = (p.top - a[2]) / dz;
                (tz0.min(tz1), tz0.max(tz1))
            };

            t0 = t0.max(0.015);
            t1 = t1.min(0.985);
            if t0 >= t1 {
                continue;
            }

            let q0x = a[0] + dx * t0;
            let q0y = a[1] + dy * t0;
            let q1x = a[0] + dx * t1;
            let q1y = a[1] + dy * t1;
            let qmx = 0.5 * (q0x + q1x);
            let qmy = 0.5 * (q0y + q1y);

            if self.point_in_prism_2d(idx, q0x, q0y)
                || self.point_in_prism_2d(idx, q1x, q1y)
                || self.point_in_prism_2d(idx, qmx, qmy)
            {
                return false;
            }

            if segment_hits_prism_edges(p, [q0x, q0y], [q1x, q1y]) {
                return false;
            }
        }

        true
    }

    #[allow(dead_code)]
    pub fn has_mortar_arc_clearance(
        &self,
        start: [f64; 3],
        vel: [f64; 3],
        gravity: f64,
        flight_t: f64,
    ) -> bool {
        if flight_t <= 0.05 {
            return true;
        }
        let samples = 8;
        let mut prev = [
            start[0] + vel[0] * (0.06 * flight_t),
            start[1] + vel[1] * (0.06 * flight_t),
            start[2] + vel[2] * (0.06 * flight_t) - 0.5 * gravity * (0.06 * flight_t).powi(2),
        ];
        for i in 1..=samples {
            let frac = 0.06 + 0.88 * (i as f64 / samples as f64);
            let t = flight_t * frac;
            let cur = [
                start[0] + vel[0] * t,
                start[1] + vel[1] * t,
                start[2] + vel[2] * t - 0.5 * gravity * t * t,
            ];
            if !self.has_los(prev, cur) {
                return false;
            }
            prev = cur;
        }
        true
    }

    fn collect_segment_prisms(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<usize> {
        let mut out = Vec::with_capacity(24);
        let fx0 = (x0 - self.grid_origin_x) / self.grid_cell;
        let fy0 = (y0 - self.grid_origin_y) / self.grid_cell;
        let fx1 = (x1 - self.grid_origin_x) / self.grid_cell;
        let fy1 = (y1 - self.grid_origin_y) / self.grid_cell;

        let mut cx = (fx0.floor() as isize).clamp(0, (self.grid_nx - 1) as isize);
        let mut cy = (fy0.floor() as isize).clamp(0, (self.grid_ny - 1) as isize);
        let end_cx = (fx1.floor() as isize).clamp(0, (self.grid_nx - 1) as isize);
        let end_cy = (fy1.floor() as isize).clamp(0, (self.grid_ny - 1) as isize);

        let dx = fx1 - fx0;
        let dy = fy1 - fy0;
        let step_x: isize = if dx > 0.0 {
            1
        } else if dx < 0.0 {
            -1
        } else {
            0
        };
        let step_y: isize = if dy > 0.0 {
            1
        } else if dy < 0.0 {
            -1
        } else {
            0
        };

        let t_delta_x = if dx.abs() > 1e-12 {
            1.0 / dx.abs()
        } else {
            f64::INFINITY
        };
        let t_delta_y = if dy.abs() > 1e-12 {
            1.0 / dy.abs()
        } else {
            f64::INFINITY
        };

        let mut t_max_x = if dx > 0.0 {
            ((cx as f64 + 1.0) - fx0) * t_delta_x
        } else if dx < 0.0 {
            (fx0 - cx as f64) * t_delta_x
        } else {
            f64::INFINITY
        };
        let mut t_max_y = if dy > 0.0 {
            ((cy as f64 + 1.0) - fy0) * t_delta_y
        } else if dy < 0.0 {
            (fy0 - cy as f64) * t_delta_y
        } else {
            f64::INFINITY
        };

        let max_iters = self.grid_nx + self.grid_ny + 4;
        for _ in 0..max_iters {
            let cell = &self.grid[cy as usize * self.grid_nx + cx as usize];
            out.extend_from_slice(cell);
            if cx == end_cx && cy == end_cy {
                break;
            }
            if t_max_x < t_max_y {
                t_max_x += t_delta_x;
                cx += step_x;
                if cx < 0 || cx >= self.grid_nx as isize {
                    break;
                }
            } else {
                t_max_y += t_delta_y;
                cy += step_y;
                if cy < 0 || cy >= self.grid_ny as isize {
                    break;
                }
            }
        }

        out.sort_unstable();
        out.dedup();
        out
    }
}

fn resolve_terrain_path(
    base_dir: &Path,
    world_path: &Path,
    terrain_file: &str,
    files: &[String],
) -> PathBuf {
    if let Some(parent) = world_path.parent() {
        let candidate = parent.join(terrain_file);
        if candidate.exists() {
            return candidate;
        }
    }
    if let Some(f) = files.iter().find(|f| f.ends_with(".i16")) {
        let candidate = base_dir.join(f);
        if candidate.exists() {
            return candidate;
        }
    }
    base_dir.join("v2/terrain.i16")
}

fn pairs_to_pts(flat: &[f64]) -> Vec<[f64; 2]> {
    flat.chunks_exact(2).map(|c| [c[0], c[1]]).collect()
}

fn point_in_ring(ring: &[[f64; 2]], x: f64, y: f64) -> bool {
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let xi = ring[i][0];
        let yi = ring[i][1];
        let xj = ring[j][0];
        let yj = ring[j][1];
        if (yi > y) != (yj > y) {
            let x_int = (xj - xi) * (y - yi) / (yj - yi) + xi;
            if x < x_int {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

fn dist_sq_point_segment(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let vx = b[0] - a[0];
    let vy = b[1] - a[1];
    let wx = p[0] - a[0];
    let wy = p[1] - a[1];
    let len2 = vx * vx + vy * vy;
    if len2 <= 1e-12 {
        return wx * wx + wy * wy;
    }
    let t = ((wx * vx + wy * vy) / len2).clamp(0.0, 1.0);
    let dx = p[0] - (a[0] + t * vx);
    let dy = p[1] - (a[1] + t * vy);
    dx * dx + dy * dy
}

fn min_dist_sq_to_ring(ring: &[[f64; 2]], x: f64, y: f64) -> f64 {
    let n = ring.len();
    if n == 0 {
        return f64::INFINITY;
    }
    let mut best = f64::INFINITY;
    let mut j = n - 1;
    for i in 0..n {
        best = best.min(dist_sq_point_segment([x, y], ring[j], ring[i]));
        j = i;
    }
    best
}

fn segment_hits_prism_edges(prism: &Prism, a: [f64; 2], b: [f64; 2]) -> bool {
    let n = prism.ring.len();
    if n < 3 {
        return false;
    }
    let mut j = n - 1;
    for i in 0..n {
        if let Some(u) = segment_intersect_param(a, b, prism.ring[j], prism.ring[i]) {
            let u_plus = (u + 1e-4).min(1.0);
            let u_minus = (u - 1e-4).max(0.0);
            let p1 = [
                a[0] + (b[0] - a[0]) * u_plus,
                a[1] + (b[1] - a[1]) * u_plus,
            ];
            let p2 = [
                a[0] + (b[0] - a[0]) * u_minus,
                a[1] + (b[1] - a[1]) * u_minus,
            ];
            let inside_solid = |pt: [f64; 2]| {
                point_in_ring(&prism.ring, pt[0], pt[1])
                    && !prism.holes.iter().any(|h| point_in_ring(h, pt[0], pt[1]))
            };
            if inside_solid(p1) || inside_solid(p2) {
                return true;
            }
        }
        j = i;
    }
    false
}

fn segment_intersect_param(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> Option<f64> {
    let r = [b[0] - a[0], b[1] - a[1]];
    let s = [d[0] - c[0], d[1] - c[1]];
    let denom = r[0] * s[1] - r[1] * s[0];
    if denom.abs() < 1e-10 {
        return None;
    }
    let qmp = [c[0] - a[0], c[1] - a[1]];
    let u = (qmp[0] * s[1] - qmp[1] * s[0]) / denom;
    let v = (qmp[0] * r[1] - qmp[1] * r[0]) / denom;
    if (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v) {
        Some(u)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_arena_and_queries_geometry() {
        let map = Map::load(
            "mechfight-kit/arena",
            &["v2/world.json".into(), "v2/terrain.i16".into()],
        )
        .expect("arena should load");
        assert_eq!(map.t_nx, 1070);
        assert_eq!(map.t_ny, 724);
        assert_eq!(map.prisms.len(), 259);
        assert_eq!(map.trees.len(), 1560);
        assert!((map.canopy_block_m - 3.96).abs() < 0.2);
        assert!(map
            .trees
            .iter()
            .all(|t| t.crown_top_z > t.crown_base_z && t.crown_r > 0.0));

        let h0 = map.ground_height(0.0, 0.0);
        assert!(h0.is_finite());

        // Building block 14 solid tower [125..-122 .. 146..-155], base 2.53, top 44.12
        let blocked = map.has_los([110.0, -140.0, 10.0], [160.0, -140.0, 10.0]);
        assert!(!blocked, "LOS should be blocked through building block 14");

        // Above the building top (44.12 m)
        let clear_above = map.has_los([110.0, -140.0, 50.0], [160.0, -140.0, 50.0]);
        assert!(clear_above, "LOS above building roof should be clear");

        // Bridge prism 0: base -1.71, top 0.29, around (-425, -57) - test open air bridge span above/below
        // Bridge prism 4: base 6.59, top 8.59 around (-115, -31)
        let under_bridge = map.has_los([-115.0, -45.0, 2.0], [-115.0, -15.0, 2.0]);
        assert!(under_bridge, "LOS under elevated bridge should be clear");
    }
}

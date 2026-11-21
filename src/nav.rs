use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

use crate::map::Map;

#[derive(Copy, Clone, Eq, PartialEq)]
struct AStarNode {
    f: u32,
    h: u32,
    idx: usize,
}

impl Ord for AStarNode {
    fn cmp(&self, other: &Self) -> Ordering {
        self.f
            .cmp(&other.f)
            .then_with(|| self.h.cmp(&other.h))
            .then_with(|| self.idx.cmp(&other.idx))
    }
}

impl PartialOrd for AStarNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub struct NavScratch {
    stamp: u32,
    seen: Vec<u32>,
    g: Vec<u32>,
    parent: Vec<usize>,
}

impl NavScratch {
    pub fn new(len: usize) -> Self {
        Self {
            stamp: 0,
            seen: vec![0; len],
            g: vec![u32::MAX; len],
            parent: vec![usize::MAX; len],
        }
    }

    fn next_run(&mut self, len: usize) -> u32 {
        if self.seen.len() != len {
            self.seen.resize(len, 0);
            self.g.resize(len, u32::MAX);
            self.parent.resize(len, usize::MAX);
        }
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            self.seen.fill(0);
            self.stamp = 1;
        }
        self.stamp
    }
}

pub struct NavGrid {
    pub cell: f64,
    pub origin_x: f64,
    pub origin_y: f64,
    pub nx: usize,
    pub ny: usize,
    pub walkable: Vec<bool>,
    pub near_cover: Vec<bool>,
    pub ground_z: Vec<f32>,
    step_up: f64,
    max_slope_tan: f64,
}

impl NavGrid {
    pub fn build(
        map: &Map,
        radius: f64,
        height: f64,
        step_up: f64,
        max_slope_deg: f64,
    ) -> Self {
        let cell = 4.0;
        let origin_x = map.min_x;
        let origin_y = map.min_y;
        let nx = (((map.max_x - map.min_x) / cell).ceil() as usize).max(1);
        let ny = (((map.max_y - map.min_y) / cell).ceil() as usize).max(1);
        let total = nx * ny;

        let mut walkable = vec![false; total];
        let mut near_cover = vec![false; total];
        let mut ground_z = vec![0.0_f32; total];

        for cy in 0..ny {
            let wy = origin_y + (cy as f64 + 0.5) * cell;
            for cx in 0..nx {
                let wx = origin_x + (cx as f64 + 0.5) * cell;
                let gz = map.ground_height(wx, wy);
                let (ok, surf_z) =
                    map.is_pos_walkable(wx, wy, gz, radius + 0.35, height, step_up);
                let idx = cy * nx + cx;
                walkable[idx] = ok;
                ground_z[idx] = surf_z as f32;
            }
        }

        const DIRS4: [(isize, isize); 4] = [(-1, 0), (1, 0), (0, -1), (0, 1)];
        for cy in 0..ny {
            let wy = origin_y + (cy as f64 + 0.5) * cell;
            for cx in 0..nx {
                let idx = cy * nx + cx;
                if !walkable[idx] {
                    continue;
                }
                let mut touches_blocked = false;
                for (dx, dy) in DIRS4 {
                    let ncx = cx as isize + dx;
                    let ncy = cy as isize + dy;
                    if ncx >= 0 && ncx < nx as isize && ncy >= 0 && ncy < ny as isize {
                        let nidx = ncy as usize * nx + ncx as usize;
                        if !walkable[nidx] {
                            touches_blocked = true;
                            break;
                        }
                    }
                }
                if touches_blocked {
                    let wx = origin_x + (cx as f64 + 0.5) * cell;
                    if map.has_tall_prism_near(wx, wy, ground_z[idx] as f64, 6.5) {
                        near_cover[idx] = true;
                    }
                }
            }
        }

        Self {
            cell,
            origin_x,
            origin_y,
            nx,
            ny,
            walkable,
            near_cover,
            ground_z,
            step_up,
            max_slope_tan: max_slope_deg.to_radians().tan(),
        }
    }

    pub fn len(&self) -> usize {
        self.nx * self.ny
    }

    #[inline]
    pub fn world_to_cell(&self, x: f64, y: f64) -> (usize, usize) {
        let cx = (((x - self.origin_x) / self.cell).floor() as isize)
            .clamp(0, (self.nx - 1) as isize) as usize;
        let cy = (((y - self.origin_y) / self.cell).floor() as isize)
            .clamp(0, (self.ny - 1) as isize) as usize;
        (cx, cy)
    }

    #[inline]
    pub fn cell_to_world(&self, cx: usize, cy: usize) -> [f64; 2] {
        [
            self.origin_x + (cx as f64 + 0.5) * self.cell,
            self.origin_y + (cy as f64 + 0.5) * self.cell,
        ]
    }

    pub fn nearest_walkable(&self, cx: usize, cy: usize, max_ring: isize) -> Option<(usize, usize)> {
        if self.walkable[cy * self.nx + cx] {
            return Some((cx, cy));
        }
        let mut best: Option<(isize, usize, usize)> = None;
        for r in 1..=max_ring {
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx.abs() != r && dy.abs() != r {
                        continue;
                    }
                    let ncx = cx as isize + dx;
                    let ncy = cy as isize + dy;
                    if ncx < 0 || ncx >= self.nx as isize || ncy < 0 || ncy >= self.ny as isize {
                        continue;
                    }
                    let ux = ncx as usize;
                    let uy = ncy as usize;
                    if self.walkable[uy * self.nx + ux] {
                        let d2 = dx * dx + dy * dy;
                        if best.is_none_or(|(bd2, _, _)| d2 < bd2) {
                            best = Some((d2, ux, uy));
                        }
                    }
                }
            }
            if let Some((_, ux, uy)) = best {
                return Some((ux, uy));
            }
        }
        None
    }

    pub fn find_path(
        &self,
        start: [f64; 2],
        goal: [f64; 2],
        max_expansions: usize,
        scratch: &mut NavScratch,
    ) -> Vec<[f64; 2]> {
        let (scx0, scy0) = self.world_to_cell(start[0], start[1]);
        let (gcx0, gcy0) = self.world_to_cell(goal[0], goal[1]);

        let Some((scx, scy)) = self.nearest_walkable(scx0, scy0, 5) else {
            return vec![goal];
        };
        let Some((gcx, gcy)) = self.nearest_walkable(gcx0, gcy0, 5) else {
            return vec![goal];
        };

        let goal_walkable = gcx == gcx0 && gcy == gcy0;
        let final_goal = if goal_walkable {
            goal
        } else {
            self.cell_to_world(gcx, gcy)
        };

        let start_idx = scy * self.nx + scx;
        let goal_idx = gcy * self.nx + gcx;
        if start_idx == goal_idx {
            return vec![final_goal];
        }

        let stamp = scratch.next_run(self.len());
        let mut heap = BinaryHeap::with_capacity(256);

        let h0 = octile_h(scx, scy, gcx, gcy);
        scratch.seen[start_idx] = stamp;
        scratch.g[start_idx] = 0;
        scratch.parent[start_idx] = start_idx;
        heap.push(Reverse(AStarNode {
            f: h0,
            h: h0,
            idx: start_idx,
        }));

        let mut best_idx = start_idx;
        let mut best_h = h0;
        let mut expansions = 0usize;
        let mut found = false;

        const DIRS8: [(isize, isize, u32, f64); 8] = [
            (1, 0, 10, 4.0),
            (-1, 0, 10, 4.0),
            (0, 1, 10, 4.0),
            (0, -1, 10, 4.0),
            (1, 1, 14, 4.0 * std::f64::consts::SQRT_2),
            (1, -1, 14, 4.0 * std::f64::consts::SQRT_2),
            (-1, 1, 14, 4.0 * std::f64::consts::SQRT_2),
            (-1, -1, 14, 4.0 * std::f64::consts::SQRT_2),
        ];

        while let Some(Reverse(node)) = heap.pop() {
            let cur_idx = node.idx;
            let cur_g = scratch.g[cur_idx];
            if node.f > cur_g.saturating_add(node.h) {
                continue;
            }
            if cur_idx == goal_idx {
                best_idx = cur_idx;
                found = true;
                break;
            }
            if node.h < best_h {
                best_h = node.h;
                best_idx = cur_idx;
            }
            expansions += 1;
            if expansions >= max_expansions {
                break;
            }

            let cx = cur_idx % self.nx;
            let cy = cur_idx / self.nx;
            let cur_z = self.ground_z[cur_idx] as f64;

            for &(dx, dy, cost, step_dist) in &DIRS8 {
                let ncx_i = cx as isize + dx;
                let ncy_i = cy as isize + dy;
                if ncx_i < 0
                    || ncx_i >= self.nx as isize
                    || ncy_i < 0
                    || ncy_i >= self.ny as isize
                {
                    continue;
                }
                let ncx = ncx_i as usize;
                let ncy = ncy_i as usize;
                let nidx = ncy * self.nx + ncx;
                if !self.walkable[nidx] {
                    continue;
                }
                if dx != 0 && dy != 0 {
                    let adj1 = cy * self.nx + ncx;
                    let adj2 = ncy * self.nx + cx;
                    if !self.walkable[adj1] || !self.walkable[adj2] {
                        continue;
                    }
                }
                let dz = (self.ground_z[nidx] as f64 - cur_z).abs();
                if dz > self.step_up && dz / step_dist > self.max_slope_tan {
                    continue;
                }

                let ng = cur_g.saturating_add(cost);
                let is_unseen = scratch.seen[nidx] != stamp;
                if is_unseen || ng < scratch.g[nidx] {
                    scratch.seen[nidx] = stamp;
                    scratch.g[nidx] = ng;
                    scratch.parent[nidx] = cur_idx;
                    let h = octile_h(ncx, ncy, gcx, gcy);
                    heap.push(Reverse(AStarNode {
                        f: ng.saturating_add(h),
                        h,
                        idx: nidx,
                    }));
                }
            }
        }

        if best_idx == start_idx {
            return vec![goal];
        }

        // Reconstruct cell sequence
        let mut cells = Vec::with_capacity(64);
        let mut cur = best_idx;
        let mut guard = 0usize;
        while cur != start_idx && guard < self.len() {
            cells.push((cur % self.nx, cur / self.nx));
            let p = scratch.parent[cur];
            if p == usize::MAX || p == cur {
                break;
            }
            cur = p;
            guard += 1;
        }
        cells.push((scx, scy));
        cells.reverse();

        // String-pull through walkable grid cells
        let mut smoothed = Vec::with_capacity(cells.len());
        let mut anchor = 0usize;
        while anchor + 1 < cells.len() {
            let mut farthest = anchor + 1;
            for j in (anchor + 2..cells.len()).rev() {
                if self.line_walkable(cells[anchor], cells[j]) {
                    farthest = j;
                    break;
                }
            }
            let pt = self.cell_to_world(cells[farthest].0, cells[farthest].1);
            smoothed.push(pt);
            anchor = farthest;
        }

        if found {
            if let Some(last) = smoothed.last_mut() {
                *last = final_goal;
            } else {
                smoothed.push(final_goal);
            }
        }
        if smoothed.is_empty() {
            smoothed.push(final_goal);
        }
        smoothed
    }

    fn line_walkable(&self, a: (usize, usize), b: (usize, usize)) -> bool {
        let p0 = self.cell_to_world(a.0, a.1);
        let p1 = self.cell_to_world(b.0, b.1);
        let dx = p1[0] - p0[0];
        let dy = p1[1] - p0[1];
        let dist = dx.hypot(dy);
        let steps = ((dist / (self.cell * 0.4)).ceil() as usize).max(1);
        let mut prev_z = self.ground_z[a.1 * self.nx + a.0] as f64;
        let step_len = dist / steps as f64;

        for s in 1..=steps {
            let t = s as f64 / steps as f64;
            let x = p0[0] + dx * t;
            let y = p0[1] + dy * t;
            let (cx, cy) = self.world_to_cell(x, y);
            let idx = cy * self.nx + cx;
            if !self.walkable[idx] {
                return false;
            }
            let z = self.ground_z[idx] as f64;
            let dz = (z - prev_z).abs();
            if dz > self.step_up && dz / step_len.max(0.5) > self.max_slope_tan {
                return false;
            }
            prev_z = z;
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    pub fn find_cover_point(
        &self,
        map: &Map,
        me_pos: [f64; 3],
        eye_h: f64,
        threats: &[[f64; 3]],
        zone_center: [f64; 2],
        zone_radius: f64,
        next_center: [f64; 2],
        next_radius: f64,
    ) -> Option<[f64; 2]> {
        self.find_cover_point_within(
            map,
            me_pos,
            eye_h,
            threats,
            zone_center,
            zone_radius,
            next_center,
            next_radius,
            64.0,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn find_cover_point_within(
        &self,
        map: &Map,
        me_pos: [f64; 3],
        eye_h: f64,
        threats: &[[f64; 3]],
        zone_center: [f64; 2],
        zone_radius: f64,
        next_center: [f64; 2],
        next_radius: f64,
        max_dist: f64,
        avoid_prev: Option<[f64; 2]>,
    ) -> Option<[f64; 2]> {
        if threats.is_empty() {
            return None;
        }
        let (me_cx, me_cy) = self.world_to_cell(me_pos[0], me_pos[1]);
        let r_cells: isize = ((max_dist / self.cell).ceil() as isize).clamp(4, 24);
        let x0 = (me_cx as isize - r_cells).max(0) as usize;
        let x1 = (me_cx as isize + r_cells).min((self.nx - 1) as isize) as usize;
        let y0 = (me_cy as isize - r_cells).max(0) as usize;
        let y1 = (me_cy as isize + r_cells).min((self.ny - 1) as isize) as usize;

        let safe_zone_r = (zone_radius - 6.0).max(0.0);
        let safe_next_r = (next_radius - 10.0).max(0.0);

        let mut cover_cands: Vec<(f64, [f64; 3], f64, bool)> = Vec::with_capacity(128);
        let mut fallback_cands: Vec<(f64, [f64; 3], f64, bool)> = Vec::with_capacity(128);

        for cy in (y0..=y1).step_by(2) {
            for cx in (x0..=x1).step_by(2) {
                let idx = cy * self.nx + cx;
                if !self.walkable[idx] {
                    continue;
                }
                let cz = self.ground_z[idx] as f64;
                if (cz - me_pos[2]).abs() > 22.0 {
                    continue;
                }
                let w = self.cell_to_world(cx, cy);
                let d_me = (w[0] - me_pos[0]).hypot(w[1] - me_pos[1]);
                if d_me < 6.0 || d_me > max_dist {
                    continue;
                }
                if (w[0] - zone_center[0]).hypot(w[1] - zone_center[1]) > safe_zone_r {
                    continue;
                }
                let in_next =
                    (w[0] - next_center[0]).hypot(w[1] - next_center[1]) <= safe_next_r;
                let mut min_d_threat = f64::INFINITY;
                for th in threats {
                    let dt = (w[0] - th[0]).hypot(w[1] - th[1]);
                    min_d_threat = min_d_threat.min(dt);
                }
                let pre = -0.8 * d_me
                    + (if in_next { 25.0 } else { 0.0 })
                    + 0.35 * min_d_threat.min(90.0);
                let item = (pre, [w[0], w[1], cz], d_me, in_next);
                if self.near_cover[idx] {
                    cover_cands.push(item);
                } else {
                    fallback_cands.push(item);
                }
            }
        }

        let mut cands = if cover_cands.len() >= 6 {
            cover_cands
        } else {
            cover_cands.extend(fallback_cands);
            cover_cands
        };
        if cands.is_empty() {
            return None;
        }
        cands.sort_unstable_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal));
        cands.truncate(24);

        // Sort threats closest-first (deterministic coordinate tie-break) and check up to 4
        let mut near_threats: Vec<[f64; 3]> = threats.to_vec();
        near_threats.sort_unstable_by(|a, b| {
            let da = (a[0] - me_pos[0]).hypot(a[1] - me_pos[1]);
            let db = (b[0] - me_pos[0]).hypot(b[1] - me_pos[1]);
            da.total_cmp(&db)
                .then_with(|| a[0].total_cmp(&b[0]))
                .then_with(|| a[1].total_cmp(&b[1]))
        });
        near_threats.truncate(4);
        let n_threats = near_threats.len();

        let mut best_pt: Option<[f64; 2]> = None;
        let mut best_score = f64::NEG_INFINITY;

        for (_, pt, d_me, in_next) in cands {
            let eye = [pt[0], pt[1], pt[2] + eye_h];
            let mut hidden = 0usize;
            let mut min_d_th = f64::INFINITY;
            for th in &near_threats {
                min_d_th = min_d_th.min((pt[0] - th[0]).hypot(pt[1] - th[1]));
                if !map.has_los(eye, *th) {
                    hidden += 1;
                }
            }
            if hidden == 0 {
                continue;
            }
            let mut score = hidden as f64 * 120.0
                + if hidden == n_threats { 80.0 } else { 0.0 }
                - 1.1 * d_me
                + if in_next { 30.0 } else { -20.0 }
                + 0.3 * min_d_th.min(80.0);
            if let Some(prev) = avoid_prev {
                if (pt[0] - prev[0]).hypot(pt[1] - prev[1]) < 16.0 {
                    score -= 180.0;
                }
            }
            if score > best_score {
                best_score = score;
                best_pt = Some([pt[0], pt[1]]);
            }
        }

        best_pt
    }

    #[allow(clippy::too_many_arguments)]
    pub fn find_cover_anchor(
        &self,
        me_pos: [f64; 3],
        zone_center: [f64; 2],
        zone_radius: f64,
        next_center: [f64; 2],
        next_radius: f64,
        avoid_prev: Option<[f64; 2]>,
        max_dist_from_me: f64,
    ) -> Option<[f64; 2]> {
        let safe_zone_r = (zone_radius - 10.0).max(0.0);
        let safe_next_r = (next_radius - 16.0).max(0.0);
        let in_next_safe = next_radius <= 24.0
            || (me_pos[0] - next_center[0]).hypot(me_pos[1] - next_center[1]) <= safe_next_r;

        let (sx, sy) = if in_next_safe {
            (me_pos[0], me_pos[1])
        } else {
            (next_center[0], next_center[1])
        };
        let (scx, scy) = self.world_to_cell(sx, sy);
        let r_cells: isize = ((max_dist_from_me / self.cell).ceil() as isize).clamp(4, 48);
        let x0 = (scx as isize - r_cells).max(0) as usize;
        let x1 = (scx as isize + r_cells).min((self.nx - 1) as isize) as usize;
        let y0 = (scy as isize - r_cells).max(0) as usize;
        let y1 = (scy as isize + r_cells).min((self.ny - 1) as isize) as usize;

        const DIRS4: [(isize, isize); 4] = [(-1, 0), (1, 0), (0, -1), (0, 1)];

        let mut best: Option<(f64, [f64; 2])> = None;

        for cy in (y0..=y1).step_by(2) {
            for cx in (x0..=x1).step_by(2) {
                let idx = cy * self.nx + cx;
                if !self.walkable[idx] || !self.near_cover[idx] {
                    continue;
                }
                let cz = self.ground_z[idx] as f64;
                if (cz - me_pos[2]).abs() > 24.0 {
                    continue;
                }
                let w = self.cell_to_world(cx, cy);
                let d_me = (w[0] - me_pos[0]).hypot(w[1] - me_pos[1]);
                if d_me > max_dist_from_me {
                    continue;
                }
                if (w[0] - zone_center[0]).hypot(w[1] - zone_center[1]) > safe_zone_r {
                    continue;
                }
                let d_next = (w[0] - next_center[0]).hypot(w[1] - next_center[1]);
                if next_radius > 24.0 && d_next > safe_next_r {
                    continue;
                }

                let mut open_nbrs = 0u8;
                let mut wall_nbrs = 0u8;
                for (dx, dy) in DIRS4 {
                    let ncx = cx as isize + dx;
                    let ncy = cy as isize + dy;
                    if ncx >= 0
                        && ncx < self.nx as isize
                        && ncy >= 0
                        && ncy < self.ny as isize
                    {
                        let nidx = ncy as usize * self.nx + ncx as usize;
                        if self.walkable[nidx] {
                            open_nbrs += 1;
                        } else {
                            wall_nbrs += 1;
                        }
                    } else {
                        wall_nbrs += 1;
                    }
                }

                let corner_bonus = if open_nbrs == 3 && wall_nbrs == 1 {
                    35.0
                } else if open_nbrs >= 2 && wall_nbrs >= 1 {
                    25.0
                } else if open_nbrs <= 1 {
                    -40.0
                } else {
                    0.0
                };

                let mut score = corner_bonus - 0.7 * d_me - 0.15 * d_next;
                if let Some(prev) = avoid_prev {
                    if (w[0] - prev[0]).hypot(w[1] - prev[1]) < 35.0 {
                        score -= 90.0;
                    }
                }

                if best.is_none_or(|(bs, _)| score > bs) {
                    best = Some((score, w));
                }
            }
        }

        best.map(|(_, pt)| pt)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn find_peek_point(
        &self,
        map: &Map,
        me_pos: [f64; 3],
        eye_h: f64,
        target_eye: [f64; 3],
        other_threats: &[[f64; 3]],
        zone_center: [f64; 2],
        zone_radius: f64,
        max_dist: f64,
    ) -> Option<[f64; 2]> {
        let (me_cx, me_cy) = self.world_to_cell(me_pos[0], me_pos[1]);
        let r_cells: isize = ((max_dist / self.cell).ceil() as isize).clamp(2, 6);
        let x0 = (me_cx as isize - r_cells).max(0) as usize;
        let x1 = (me_cx as isize + r_cells).min((self.nx - 1) as isize) as usize;
        let y0 = (me_cy as isize - r_cells).max(0) as usize;
        let y1 = (me_cy as isize + r_cells).min((self.ny - 1) as isize) as usize;

        let safe_zone_r = (zone_radius - 6.0).max(0.0);
        let mut cands: Vec<(f64, [f64; 3], f64, bool)> = Vec::with_capacity(64);

        for cy in y0..=y1 {
            for cx in x0..=x1 {
                let idx = cy * self.nx + cx;
                if !self.walkable[idx] {
                    continue;
                }
                let cz = self.ground_z[idx] as f64;
                if (cz - me_pos[2]).abs() > 14.0 {
                    continue;
                }
                let w = self.cell_to_world(cx, cy);
                let d_me = (w[0] - me_pos[0]).hypot(w[1] - me_pos[1]);
                if d_me < 2.0 || d_me > max_dist {
                    continue;
                }
                if (w[0] - zone_center[0]).hypot(w[1] - zone_center[1]) > safe_zone_r {
                    continue;
                }
                let near_cov = self.near_cover[idx];
                let pre = d_me - if near_cov { 4.0 } else { 0.0 };
                cands.push((pre, [w[0], w[1], cz], d_me, near_cov));
            }
        }

        if cands.is_empty() {
            return None;
        }
        cands.sort_unstable_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
        cands.truncate(20);

        let mut near_others: Vec<[f64; 3]> = other_threats.to_vec();
        near_others.sort_unstable_by(|a, b| {
            let da = (a[0] - me_pos[0]).hypot(a[1] - me_pos[1]);
            let db = (b[0] - me_pos[0]).hypot(b[1] - me_pos[1]);
            da.total_cmp(&db)
                .then_with(|| a[0].total_cmp(&b[0]))
                .then_with(|| a[1].total_cmp(&b[1]))
        });
        near_others.truncate(4);

        let mut best_pt: Option<[f64; 2]> = None;
        let mut best_score = f64::NEG_INFINITY;

        for (_, pt, d_me, near_cov) in cands {
            let eye = [pt[0], pt[1], pt[2] + eye_h];
            if !map.has_los(eye, target_eye) {
                continue;
            }
            if near_others.iter().any(|ot| map.has_los(eye, *ot)) {
                continue;
            }
            let score = -1.8 * d_me + if near_cov { 24.0 } else { 0.0 };
            if score > best_score {
                best_score = score;
                best_pt = Some([pt[0], pt[1]]);
            }
        }

        best_pt
    }

    pub fn has_near_cover_within(&self, pos: [f64; 3], max_dist: f64) -> bool {
        let (cx0, cy0) = self.world_to_cell(pos[0], pos[1]);
        let r: isize = ((max_dist / self.cell).ceil() as isize).clamp(1, 8);
        let x0 = (cx0 as isize - r).max(0) as usize;
        let x1 = (cx0 as isize + r).min((self.nx - 1) as isize) as usize;
        let y0 = (cy0 as isize - r).max(0) as usize;
        let y1 = (cy0 as isize + r).min((self.ny - 1) as isize) as usize;
        for cy in y0..=y1 {
            for cx in x0..=x1 {
                let idx = cy * self.nx + cx;
                if self.walkable[idx]
                    && self.near_cover[idx]
                    && (self.ground_z[idx] as f64 - pos[2]).abs() <= 16.0
                {
                    let w = self.cell_to_world(cx, cy);
                    if (w[0] - pos[0]).hypot(w[1] - pos[1]) <= max_dist {
                        return true;
                    }
                }
            }
        }
        false
    }
}

#[inline]
fn octile_h(x0: usize, y0: usize, x1: usize, y1: usize) -> u32 {
    let dx = x0.abs_diff(x1) as u32;
    let dy = y0.abs_diff(y1) as u32;
    let (mn, mx) = if dx < dy { (dx, dy) } else { (dy, dx) };
    14 * mn + 10 * (mx - mn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn builds_nav_grid_and_finds_path_and_cover() {
        let map = Map::load(
            "mechfight-kit/arena",
            &["v2/world.json".into(), "v2/terrain.i16".into()],
        )
        .expect("arena should load");
        let t0 = Instant::now();
        let nav = NavGrid::build(&map, 2.5, 9.0, 1.2, 50.0);
        let build_ms = t0.elapsed().as_secs_f64() * 1000.0;
        assert!(build_ms < 500.0, "NavGrid build took {build_ms:.1} ms");

        // Navigate across building block 14 (90.6..186.0, -175.9..-101.7)
        let mut scratch = NavScratch::new(nav.len());
        let start = [80.0, -140.0];
        let goal = [196.0, -140.0];
        let path = nav.find_path(start, goal, 2600, &mut scratch);
        assert!(path.len() >= 2, "path should bend around building block 14: {path:?}");
        assert_eq!(*path.last().unwrap(), goal);
        for wp in &path {
            let (cx, cy) = nav.world_to_cell(wp[0], wp[1]);
            assert!(nav.walkable[cy * nav.nx + cx], "waypoint {wp:?} must be walkable");
        }

        // Cover finder near block 14 against threat at [75, -140]
        let me_z = map.ground_height(86.0, -110.0);
        let th_z = map.ground_height(75.0, -140.0);
        let threat = [75.0, -140.0, th_z + 8.0];
        let cover = nav
            .find_cover_point(
                &map,
                [86.0, -110.0, me_z],
                8.0,
                &[threat],
                [0.0, 0.0],
                500.0,
                [0.0, 0.0],
                400.0,
            )
            .expect("should find cover behind building");
        let cz = map.ground_height(cover[0], cover[1]);
        assert!(
            !map.has_los([cover[0], cover[1], cz + 8.0], threat),
            "cover point {cover:?} must block LOS from threat"
        );
    }

    #[test]
    fn find_cover_anchor_and_peek_point_on_arena_map() {
        let map = Map::load(
            "mechfight-kit/arena",
            &["v2/world.json".into(), "v2/terrain.i16".into()],
        )
        .expect("arena should load");
        let nav = NavGrid::build(&map, 2.5, 9.0, 1.2, 50.0);

        let me_z = map.ground_height(86.0, -110.0);
        let next_center = [50.0, -100.0];
        let next_radius = 220.0;
        let anchor = nav
            .find_cover_anchor(
                [86.0, -110.0, me_z],
                [50.0, -100.0],
                400.0,
                next_center,
                next_radius,
                None,
                160.0,
            )
            .expect("should find cover anchor inside next_zone");
        let (acx, acy) = nav.world_to_cell(anchor[0], anchor[1]);
        let aidx = acy * nav.nx + acx;
        assert!(nav.walkable[aidx], "anchor must be walkable");
        assert!(nav.near_cover[aidx], "anchor must be near_cover");
        let d_next = (anchor[0] - next_center[0]).hypot(anchor[1] - next_center[1]);
        assert!(
            d_next <= next_radius - 16.0 + 1e-6,
            "anchor must lie inside safe next_zone ({d_next:.1})"
        );

        let th_z = map.ground_height(75.0, -140.0);
        let threat = [75.0, -140.0, th_z + 8.0];
        let cover = nav
            .find_cover_point(
                &map,
                [86.0, -110.0, me_z],
                8.0,
                &[threat],
                [0.0, 0.0],
                500.0,
                [0.0, 0.0],
                400.0,
            )
            .expect("should find cover behind building");
        let cz = map.ground_height(cover[0], cover[1]);
        let peek = nav
            .find_peek_point(
                &map,
                [cover[0], cover[1], cz],
                8.0,
                threat,
                &[],
                [0.0, 0.0],
                500.0,
                18.0,
            )
            .expect("should find peek point around corner with LOS to threat");
        let pz = map.ground_height(peek[0], peek[1]);
        assert!(
            map.has_los([peek[0], peek[1], pz + 8.0], threat),
            "peek point {peek:?} must have 3D LOS to threat"
        );
    }
}

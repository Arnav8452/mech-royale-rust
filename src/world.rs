use std::collections::{HashMap, HashSet, VecDeque};

use crate::geom::{angle_diff, dist2d, dist3d, is_ground_level};
use crate::map::Map;
use crate::protocol::{stat, Config, GameEvent, Observation, WeaponKind};

#[derive(Clone, Debug)]
pub(crate) struct TrackedEnemy {
    pub pos: [f64; 3],
    pub vel: [f64; 3],
    pub est_accel_z: f64,
    pub aim: [f64; 2],
    #[allow(dead_code)]
    pub hp: f64,
    #[allow(dead_code)]
    pub max_hp: f64,
    pub weapons: Vec<WeaponKind>,
    pub charging: bool,
    pub last_seen_t: f64,
    pub gun_reload_until: f64,
    pub laser_cd_until: f64,
    pub est_gun_shots: u8,
    pub last_gun_shot_t: f64,
    pub last_reversal_t: f64,
}

impl TrackedEnemy {
    #[inline]
    pub fn is_reloading(&self, t: f64) -> bool {
        t < self.gun_reload_until - 0.15
    }

    #[inline]
    pub fn is_laser_on_cd(&self, t: f64) -> bool {
        t < self.laser_cd_until - 0.15
    }

    #[inline]
    #[allow(dead_code)]
    pub fn reload_remaining(&self, t: f64) -> f64 {
        (self.gun_reload_until - t).max(0.0)
    }

    #[inline]
    #[allow(dead_code)]
    pub fn laser_cd_remaining(&self, t: f64) -> f64 {
        (self.laser_cd_until - t).max(0.0)
    }

    #[inline]
    pub fn is_vulnerable(&self, t: f64) -> bool {
        let has_gun = self.weapons.contains(&WeaponKind::Gun);
        let has_laser = self.weapons.contains(&WeaponKind::Laser);
        (has_gun && self.is_reloading(t)) || (has_laser && self.is_laser_on_cd(t) && !self.charging)
    }
}

pub(crate) struct WorldState {
    pub tracked_enemies: HashMap<u64, TrackedEnemy>,
    pub visited_cells: HashSet<(i32, i32)>,
    pub last_hunt_pos: Option<([f64; 2], f64)>,
    pub seen_proj_ids: HashMap<u64, f64>,
    pub last_pickup_seen: HashMap<u64, (WeaponKind, [f64; 3], f64)>,
    pub blacklisted_pickups: HashMap<u64, f64>,
    pub last_damage_bearing: Option<f64>,
    pub last_damage_t: f64,
    pub last_mech_damage_t: f64,
    pub last_damage_by: Option<u64>,
    pub last_target_id: Option<u64>,
    pub last_sound_bearing: Option<f64>,
    pub last_sound_t: f64,
    pub last_sound_priority: f64,
    pub last_sound_is_near: bool,
    pub last_sound_kind: String,
    pub last_sound_dist: String,
    pub last_move: [f64; 2],
    pub strafe_sign: f64,
    pub last_strafe_flip_t: f64,
    pub next_strafe_flip_t: f64,
    pub juke_boost_until: f64,
    pub orbit_active_1v1: bool,
    pub orbit_sign: f64,
    pub last_orbit_flip_t: f64,
    pub roof_climb_goal: Option<([f64; 2], f64)>,
    pub roof_climb_start_t: f64,
    pub roof_climb_cooldown_until: f64,
    pub last_roof_los_t: f64,
    rng_state: u64,
    pub pos_history: VecDeque<[f64; 2]>,
    pub cmd_move_streak: u32,
    pub unstuck_air_boost_until: f64,
    pub last_combat_t: f64,
    pub last_scan_dir: f64,
}

impl WorldState {
    pub fn new(seed: u64) -> Self {
        let strafe_sign = if seed % 2 == 0 { 1.0 } else { -1.0 };
        Self {
            tracked_enemies: HashMap::new(),
            visited_cells: HashSet::new(),
            last_hunt_pos: None,
            seen_proj_ids: HashMap::new(),
            last_pickup_seen: HashMap::new(),
            blacklisted_pickups: HashMap::new(),
            last_damage_bearing: None,
            last_damage_t: -100.0,
            last_mech_damage_t: -100.0,
            last_damage_by: None,
            last_target_id: None,
            last_sound_bearing: None,
            last_sound_t: -100.0,
            last_sound_priority: 0.0,
            last_sound_is_near: false,
            last_sound_kind: String::new(),
            last_sound_dist: String::new(),
            last_move: [0.0, 0.0],
            strafe_sign,
            last_strafe_flip_t: -100.0,
            next_strafe_flip_t: -100.0,
            juke_boost_until: -100.0,
            orbit_active_1v1: false,
            orbit_sign: strafe_sign,
            last_orbit_flip_t: -100.0,
            roof_climb_goal: None,
            roof_climb_start_t: -100.0,
            roof_climb_cooldown_until: -100.0,
            last_roof_los_t: -100.0,
            rng_state: (seed ^ 0x9E37_79B9_7F4A_7C15).max(1),
            pos_history: VecDeque::with_capacity(7),
            cmd_move_streak: 0,
            unstuck_air_boost_until: -100.0,
            last_combat_t: -100.0,
            last_scan_dir: 1.0,
        }
    }

    pub fn update(&mut self, obs: &Observation, map: &Map, config: &Config) -> bool {
        let t = obs.t;
        let me = &obs.self_obs;
        let eye_h = stat(&me.stats, "eye_height", 8.0);
        let eye = [me.pos[0], me.pos[1], me.pos[2] + eye_h];

        let gun_reload_s = stat(&config.weapons.gun, "reload", 2.4);
        let gun_mag = stat(&config.weapons.gun, "mag", 12.0).round().max(1.0) as u8;
        let laser_cd_s = stat(&config.weapons.laser, "cooldown", 1.65);

        self.visited_cells.insert((
            (me.pos[0] / 50.0).floor() as i32,
            (me.pos[1] / 50.0).floor() as i32,
        ));
        if let Some((hpos, ht)) = self.last_hunt_pos {
            if t - ht > 15.0 || dist2d(me.pos[0], me.pos[1], hpos[0], hpos[1]) <= 22.0 {
                self.last_hunt_pos = None;
            }
        }

        self.blacklisted_pickups.retain(|_, exp_t| t - *exp_t <= 15.0);

        for ev in &obs.events {
            match ev {
                GameEvent::Damaged {
                    weapon,
                    bearing,
                    by,
                    ..
                } => {
                    self.last_combat_t = t;
                    if let Some(b) = bearing {
                        self.last_damage_bearing = Some(*b);
                        self.last_damage_t = t;
                    }
                    if weapon != "npc" && weapon != "zone" {
                        self.last_mech_damage_t = t;
                        if by.is_some() {
                            self.last_damage_by = *by;
                        }
                    }
                    if weapon == "gun" || weapon == "npc" {
                        if self.orbit_active_1v1 {
                            self.juke_boost_until = t + 0.30;
                        } else if t - self.last_strafe_flip_t >= 0.35 {
                            self.strafe_sign = -self.strafe_sign;
                            self.last_strafe_flip_t = t;
                            self.next_strafe_flip_t = t + 0.65 + 0.40 * self.next_unit_f64();
                            self.juke_boost_until = t + 0.22;
                        }
                    }
                }
                GameEvent::Hit { .. } => {
                    self.last_combat_t = t;
                }
                GameEvent::Took { id, .. } => {
                    self.last_pickup_seen.remove(id);
                }
                GameEvent::TakeFailed { id, reason } => {
                    if reason == "gone" || reason == "have" {
                        self.last_pickup_seen.remove(id);
                        self.blacklisted_pickups.insert(*id, t);
                    }
                }
                GameEvent::Kill { target } => {
                    self.tracked_enemies.remove(target);
                    if self.last_target_id == Some(*target) {
                        self.last_target_id = None;
                    }
                    if self.tracked_enemies.is_empty() {
                        self.last_hunt_pos = None;
                    }
                }
                _ => {}
            }
        }

        // Track enemy mechs with vertical acceleration estimation & cooldown preservation
        for actor in &obs.mechs {
            if actor.kind != "mech" {
                continue;
            }
            let (
                est_az,
                mut gun_reload_until,
                mut laser_cd_until,
                est_gun_shots,
                last_gun_shot_t,
                last_reversal_t,
            ) = if let Some(prev) = self.tracked_enemies.get(&actor.id) {
                let dt = t - prev.last_seen_t;
                let az = if (0.04..=0.5).contains(&dt) {
                    0.5 * prev.est_accel_z + 0.5 * ((actor.vel[2] - prev.vel[2]) / dt)
                } else {
                    0.0
                };
                let mut lcd = prev.laser_cd_until;
                if prev.charging && !actor.charging && dt <= 0.35 {
                    lcd = lcd.max(t + laser_cd_s);
                }
                let prev_spd = prev.vel[0].hypot(prev.vel[1]);
                let cur_spd = actor.vel[0].hypot(actor.vel[1]);
                let rev = if (0.04..=0.5).contains(&dt)
                    && prev_spd >= 4.0
                    && cur_spd >= 4.0
                    && (prev.vel[0] * actor.vel[0] + prev.vel[1] * actor.vel[1]) < -8.0
                {
                    t
                } else {
                    prev.last_reversal_t
                };
                (
                    az,
                    prev.gun_reload_until,
                    lcd,
                    prev.est_gun_shots,
                    prev.last_gun_shot_t,
                    rev,
                )
            } else {
                (0.0, -100.0, -100.0, 0, -100.0, -100.0)
            };

            if !actor.weapons.contains(&WeaponKind::Gun) {
                gun_reload_until = -100.0;
            }
            if !actor.weapons.contains(&WeaponKind::Laser) {
                laser_cd_until = -100.0;
            }

            self.tracked_enemies.insert(
                actor.id,
                TrackedEnemy {
                    pos: actor.pos,
                    vel: actor.vel,
                    est_accel_z: est_az,
                    aim: actor.aim,
                    hp: actor.hp.unwrap_or(1400.0),
                    max_hp: actor.max_hp.unwrap_or(1400.0),
                    weapons: actor.weapons.clone(),
                    charging: actor.charging,
                    last_seen_t: t,
                    gun_reload_until,
                    laser_cd_until,
                    est_gun_shots,
                    last_gun_shot_t,
                    last_reversal_t,
                },
            );
        }
        for e in self.tracked_enemies.values() {
            if t - e.last_seen_t > 4.5 {
                let dominated = self
                    .last_hunt_pos
                    .is_some_and(|(_, ht)| ht >= e.last_seen_t);
                if !dominated {
                    self.last_hunt_pos = Some(([e.pos[0], e.pos[1]], e.last_seen_t));
                }
            }
        }
        self.tracked_enemies.retain(|_, e| t - e.last_seen_t <= 4.5);
        if !self.tracked_enemies.is_empty() {
            self.last_hunt_pos = None;
        }
        if let Some(tid) = self.last_target_id {
            if !self.tracked_enemies.contains_key(&tid) {
                self.last_target_id = None;
            }
        }

        // Track observed enemy laser/lightning beams: pinpoint 1.65 s laser cooldown + discover unseen shooters
        for b in &obs.beams {
            if b.by == me.id || b.by >= 100 {
                continue;
            }
            let beam_weapon = match b.kind.as_str() {
                "laser" => Some(WeaponKind::Laser),
                "lightning" => Some(WeaponKind::Lightning),
                _ => None,
            };
            let beam_t = t - b.age;
            if let Some(e) = self.tracked_enemies.get_mut(&b.by) {
                if b.kind == "laser" {
                    e.laser_cd_until = e.laser_cd_until.max(beam_t + laser_cd_s);
                }
                if let Some(wk) = beam_weapon {
                    if !e.weapons.contains(&wk) {
                        e.weapons.push(wk);
                    }
                }
                if t - e.last_seen_t > 0.25 {
                    if let Some(&mpos) = b.path.first() {
                        let feet_z = map
                            .surface_height(mpos[0], mpos[1], mpos[2])
                            .min(mpos[2] - 4.0);
                        e.pos = [mpos[0], mpos[1], feet_z];
                        e.last_seen_t = beam_t.max(e.last_seen_t);
                    }
                }
            } else if let Some(&mpos) = b.path.first() {
                let feet_z = map
                    .surface_height(mpos[0], mpos[1], mpos[2])
                    .min(mpos[2] - 4.0);
                self.tracked_enemies.insert(
                    b.by,
                    TrackedEnemy {
                        pos: [mpos[0], mpos[1], feet_z],
                        vel: [0.0, 0.0, 0.0],
                        est_accel_z: 0.0,
                        aim: [0.0, 0.0],
                        hp: 1400.0,
                        max_hp: 1400.0,
                        weapons: beam_weapon.into_iter().collect(),
                        charging: false,
                        last_seen_t: beam_t,
                        gun_reload_until: -100.0,
                        laser_cd_until: if b.kind == "laser" {
                            beam_t + laser_cd_s
                        } else {
                            -100.0
                        },
                        est_gun_shots: 0,
                        last_gun_shot_t: -100.0,
                        last_reversal_t: -100.0,
                    },
                );
            }
        }

        // Global drop-holder radar: any enemy holding a landed drop is known to be inside its 8 m ring
        for d in &obs.drops {
            if d.lands_in <= 0.0 {
                if let Some(hid) = d.holder {
                    if hid != me.id && hid < 100 {
                        if let Some(e) = self.tracked_enemies.get_mut(&hid) {
                            if t - e.last_seen_t > 0.25 {
                                e.pos = d.pos;
                                e.last_seen_t = t;
                            }
                        } else {
                            self.tracked_enemies.insert(
                                hid,
                                TrackedEnemy {
                                    pos: d.pos,
                                    vel: [0.0, 0.0, 0.0],
                                    est_accel_z: 0.0,
                                    aim: [0.0, 0.0],
                                    hp: 1400.0,
                                    max_hp: 1400.0,
                                    weapons: Vec::new(),
                                    charging: false,
                                    last_seen_t: t,
                                    gun_reload_until: -100.0,
                                    laser_cd_until: -100.0,
                                    est_gun_shots: 0,
                                    last_gun_shot_t: -100.0,
                                    last_reversal_t: -100.0,
                                },
                            );
                        }
                    }
                }
            }
        }

        // Track visible enemy gun projectiles near muzzles to predict full-mag reloads
        self.seen_proj_ids.retain(|_, seen_t| t - *seen_t <= 3.0);
        for p in &obs.projectiles {
            if p.kind != "gun" || self.seen_proj_ids.contains_key(&p.id) {
                continue;
            }
            self.seen_proj_ids.insert(p.id, t);

            let p_horiz = p.vel[0].hypot(p.vel[1]);
            if p_horiz <= 1.0 {
                continue;
            }
            let pv_dir = [p.vel[0] / p_horiz, p.vel[1] / p_horiz];

            let mut best_shooter: Option<(f64, u64)> = None;
            for (&id, e) in &self.tracked_enemies {
                if !e.weapons.contains(&WeaponKind::Gun)
                    || t - e.last_seen_t > 0.4
                    || t < e.gun_reload_until - 0.1
                {
                    continue;
                }
                let e_muzzle = [e.pos[0], e.pos[1], e.pos[2] + 7.0];
                let d = dist3d(e_muzzle, p.pos);
                if d > 14.0 {
                    continue;
                }
                let aim_dir = [e.aim[0].cos(), e.aim[0].sin()];
                let dot = pv_dir[0] * aim_dir[0] + pv_dir[1] * aim_dir[1];
                if dot > 0.65 && best_shooter.is_none_or(|(bd, _)| d < bd) {
                    best_shooter = Some((d, id));
                }
            }

            if let Some((_, shooter_id)) = best_shooter {
                if let Some(e) = self.tracked_enemies.get_mut(&shooter_id) {
                    if t - e.last_gun_shot_t > 2.2 {
                        e.est_gun_shots = 0;
                    }
                    e.est_gun_shots = e.est_gun_shots.saturating_add(1);
                    e.last_gun_shot_t = t;
                    if e.est_gun_shots >= gun_mag {
                        e.gun_reload_until = e.gun_reload_until.max(t + gun_reload_s);
                        e.est_gun_shots = 0;
                    }
                }
            }
        }

        // Remember reachable ground-level non-mortar weapon pickups spotted during scans
        let visible_pickup_ids: Vec<u64> = obs.pickups.iter().map(|p| p.id).collect();
        self.last_pickup_seen.retain(|id, (_, pos, seen_t)| {
            if self.blacklisted_pickups.contains_key(id) || t - *seen_t > 12.0 {
                return false;
            }
            let d = dist2d(me.pos[0], me.pos[1], pos[0], pos[1]);
            if !visible_pickup_ids.contains(id)
                && (d <= 6.0 || (d <= 35.0 && map.has_los(eye, [pos[0], pos[1], pos[2] + 0.5])))
            {
                return false;
            }
            true
        });
        for p in &obs.pickups {
            if p.kind == WeaponKind::Mortar || self.blacklisted_pickups.contains_key(&p.id) {
                continue;
            }
            let gz = map.ground_height(p.pos[0], p.pos[1]);
            let dh = p.pos[2] - gz;
            if is_ground_level(map, p.pos)
                || ((2.5..=40.0).contains(&dh) && map.roof_top_at(p.pos[0], p.pos[1]).is_some())
            {
                self.last_pickup_seen.insert(p.id, (p.kind, p.pos, t));
            }
        }

        // Decay sound memory priority & attribute reload sounds to nearby tracked enemies
        if t - self.last_sound_t > 2.5 {
            self.last_sound_priority = 0.0;
            self.last_sound_is_near = false;
        }
        for s in &obs.sounds {
            if s.kind == "reload" {
                let mut best_match: Option<(f64, u64)> = None;
                for (&id, e) in &self.tracked_enemies {
                    if !e.weapons.contains(&WeaponKind::Gun) {
                        continue;
                    }
                    let d = dist2d(me.pos[0], me.pos[1], e.pos[0], e.pos[1]);
                    if d > 75.0 {
                        continue;
                    }
                    let e_bearing = (e.pos[1] - me.pos[1]).atan2(e.pos[0] - me.pos[0]);
                    let err = angle_diff(s.bearing, e_bearing).abs();
                    if err <= 0.30 && best_match.is_none_or(|(be, _)| err < be) {
                        best_match = Some((err, id));
                    }
                }
                if let Some((_, eid)) = best_match {
                    if let Some(e) = self.tracked_enemies.get_mut(&eid) {
                        e.gun_reload_until = e.gun_reload_until.max((t - s.age) + gun_reload_s);
                        e.est_gun_shots = 0;
                    }
                }
            }

            let s_time = t - s.age;
            if s_time < self.last_sound_t - 0.5 && self.last_sound_priority > 0.0 {
                continue;
            }
            let is_near = s.dist == "near";
            let dist_w = match s.dist.as_str() {
                "near" => 3.0,
                "mid" => 1.5,
                _ => 0.5,
            };
            let kind_w = match s.kind.as_str() {
                "laser" | "laser_charge" | "lightning" | "gun" | "mortar" | "explosion" => 1.5,
                "boost" | "reload" => 1.1,
                _ => 0.8,
            };
            let prio = dist_w * kind_w;
            if prio >= self.last_sound_priority || t - self.last_sound_t > 1.2 {
                self.last_sound_bearing = Some(s.bearing);
                self.last_sound_t = s_time;
                self.last_sound_priority = prio;
                self.last_sound_is_near = is_near;
                self.last_sound_kind = s.kind.clone();
                self.last_sound_dist = s.dist.clone();
            }
        }

        // 5-tick displacement anti-stuck tracking
        if self.last_move[0].hypot(self.last_move[1]) > 0.5 {
            self.cmd_move_streak = self.cmd_move_streak.saturating_add(1);
        } else {
            self.cmd_move_streak = 0;
        }
        if self.pos_history.len() >= 6 {
            self.pos_history.pop_front();
        }
        self.pos_history.push_back([me.pos[0], me.pos[1]]);

        let stuck_active = if self.cmd_move_streak >= 5 && self.pos_history.len() >= 6 {
            if let Some(&oldest) = self.pos_history.front() {
                dist2d(me.pos[0], me.pos[1], oldest[0], oldest[1]) < 0.8
            } else {
                false
            }
        } else {
            false
        };

        // Jittered fast A-D strafe reversals (0.65-1.05 s) to break linear Gun lead prediction
        if t >= self.next_strafe_flip_t {
            if self.next_strafe_flip_t > -50.0 {
                self.strafe_sign = -self.strafe_sign;
            }
            self.last_strafe_flip_t = t;
            self.next_strafe_flip_t = t + 0.65 + 0.40 * self.next_unit_f64();
        }

        stuck_active
    }

    fn next_unit_f64(&mut self) -> f64 {
        let mut x = self.rng_state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng_state = x.max(1);
        ((x >> 11) as f64) / ((1u64 << 53) as f64)
    }

    pub fn closest_tracked_enemy_within(
        &self,
        me_pos: [f64; 3],
        max_d: f64,
    ) -> Option<(u64, &TrackedEnemy)> {
        self.tracked_enemies
            .iter()
            .filter(|(_, e)| dist2d(me_pos[0], me_pos[1], e.pos[0], e.pos[1]) <= max_d)
            .min_by(|(ida, a), (idb, b)| {
                let da = dist2d(me_pos[0], me_pos[1], a.pos[0], a.pos[1]);
                let db = dist2d(me_pos[0], me_pos[1], b.pos[0], b.pos[1]);
                da.total_cmp(&db).then_with(|| ida.cmp(idb))
            })
            .map(|(&id, e)| (id, e))
    }

    pub fn is_huntable_sound(&self, t: f64) -> bool {
        self.last_sound_bearing.is_some()
            && t - self.last_sound_t <= 4.0
            && matches!(
                self.last_sound_kind.as_str(),
                "gun"
                    | "laser"
                    | "laser_charge"
                    | "lightning"
                    | "mortar"
                    | "explosion"
                    | "crate"
                    | "npc_gun"
                    | "step"
                    | "boost"
                    | "reload"
            )
    }
}

use std::collections::{HashMap, VecDeque};
use std::f64::consts::PI;

use crate::map::Map;
use crate::nav::{NavGrid, NavScratch};
use crate::protocol::{
    stat, Action, Config, DebugDraw, DebugLine, DebugPoint, DropObs, GameEvent, Hello,
    Observation, WeaponKind, WeaponObs,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Posture {
    ZoneRotate,
    EmergencyRegen,
    SupplyDrop,
    LootWeapon,
    Combat,
    EarlyFarm,
}

impl Posture {
    fn tag(self) -> &'static str {
        match self {
            Posture::ZoneRotate => "ZON",
            Posture::EmergencyRegen => "REG",
            Posture::SupplyDrop => "DRP",
            Posture::LootWeapon => "LOT",
            Posture::Combat => "CMB",
            Posture::EarlyFarm => "FRM",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TargetKind {
    Mech,
    Npc,
    Crate,
}

#[derive(Clone)]
struct TrackedEnemy {
    pos: [f64; 3],
    vel: [f64; 3],
    est_accel_z: f64,
    last_seen_t: f64,
}

#[derive(Clone)]
struct TargetCandidate {
    id: u64,
    kind: TargetKind,
    pos: [f64; 3],
    vel: [f64; 3],
    est_accel_z: f64,
    aim: [f64; 2],
    com_z: f64,
    radius: f64,
    hp: f64,
    max_hp: f64,
    weapons: Vec<WeaponKind>,
    charging: bool,
    has_los: bool,
    dist: f64,
}

pub struct Bot {
    name: String,
    config: Config,
    map: Map,
    nav: NavGrid,
    nav_scratch: NavScratch,
    cached_path: Vec<[f64; 2]>,
    cached_path_goal: [f64; 2],
    cached_path_t: f64,
    posture: Posture,
    cover_target: Option<[f64; 2]>,
    prev_cover_target: Option<[f64; 2]>,
    cover_target_t: f64,
    tracked_enemies: HashMap<u64, TrackedEnemy>,
    last_pickup_seen: HashMap<u64, (WeaponKind, [f64; 3], f64)>,
    blacklisted_pickups: HashMap<u64, f64>,
    last_damage_bearing: Option<f64>,
    last_damage_t: f64,
    last_damage_by: Option<u64>,
    last_sound_bearing: Option<f64>,
    last_sound_t: f64,
    last_sound_priority: f64,
    last_sound_is_near: bool,
    last_move: [f64; 2],
    strafe_sign: f64,
    last_strafe_flip_t: f64,
    pos_history: VecDeque<[f64; 2]>,
    cmd_move_streak: u32,
    unstuck_air_boost_until: f64,
    last_combat_t: f64,
    last_scan_dir: f64,
}

impl Bot {
    pub fn new(hello: Hello) -> Self {
        let map = match Map::load(&hello.map.dir, &hello.map.files) {
            Ok(m) => m,
            Err(err) => {
                eprintln!("warning: map load failed ({err}); using fallback empty map");
                Map::fallback_empty()
            }
        };
        let radius = stat(&hello.config.mech, "radius", 2.5);
        let height = stat(&hello.config.mech, "height", 9.0);
        let step_up = stat(&hello.config.mech, "step_up", 1.2);
        let max_slope_deg = stat(&hello.config.mech, "max_slope_deg", 50.0);
        let nav = NavGrid::build(&map, radius, height, step_up, max_slope_deg);
        let nav_scratch = NavScratch::new(nav.len());
        let strafe_sign = if hello.seed % 2 == 0 { 1.0 } else { -1.0 };
        Self {
            name: "rust-bot-m3".to_string(),
            config: hello.config,
            map,
            nav,
            nav_scratch,
            cached_path: Vec::new(),
            cached_path_goal: [0.0, 0.0],
            cached_path_t: -100.0,
            posture: Posture::EarlyFarm,
            cover_target: None,
            prev_cover_target: None,
            cover_target_t: -100.0,
            tracked_enemies: HashMap::new(),
            last_pickup_seen: HashMap::new(),
            blacklisted_pickups: HashMap::new(),
            last_damage_bearing: None,
            last_damage_t: -100.0,
            last_damage_by: None,
            last_sound_bearing: None,
            last_sound_t: -100.0,
            last_sound_priority: 0.0,
            last_sound_is_near: false,
            last_move: [0.0, 0.0],
            strafe_sign,
            last_strafe_flip_t: -100.0,
            pos_history: VecDeque::with_capacity(7),
            cmd_move_streak: 0,
            unstuck_air_boost_until: -100.0,
            last_combat_t: -100.0,
            last_scan_dir: 1.0,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn act(&mut self, obs: &Observation) -> Action {
        let me = &obs.self_obs;
        let t = obs.t;

        let stuck_active = self.update_memory(obs);

        let mut action = Action {
            tick: obs.tick,
            ..Default::default()
        };

        if let Some(offer) = &obs.offer {
            action.pick = Some(self.pick_upgrade(offer, me));
        }

        let muzzle_h = stat(&me.stats, "muzzle_height", 7.0);
        let eye_h = stat(&me.stats, "eye_height", 8.0);
        let muzzle = [me.pos[0], me.pos[1], me.pos[2] + muzzle_h];
        let mech_radius = stat(&me.stats, "radius", 2.5);
        let mech_height = stat(&me.stats, "height", 9.0);
        let min_pitch = stat(&me.stats, "min_pitch", -0.7);
        let max_pitch = stat(&me.stats, "max_pitch", 1.3);

        let (mech_cands, primary_farm) = self.select_targets(obs, muzzle);
        let los_mechs: Vec<&TargetCandidate> = mech_cands.iter().filter(|c| c.has_los).collect();
        let los_mech_count = los_mechs.len();
        let near_mech_count = self
            .tracked_enemies
            .values()
            .filter(|e| dist2d(me.pos[0], me.pos[1], e.pos[0], e.pos[1]) <= 180.0)
            .count();

        let (take_id, drop_slot) = self.pick_weapon_take_and_drop(obs, &los_mechs);
        if let Some(pid) = take_id {
            action.take = Some(pid);
            if let Some(ds) = drop_slot {
                action.drop = Some(ds);
            }
        }

        let immediate_kill_avail = me.hp > 220.0 && los_mechs.iter().any(|m| m.hp <= 420.0);

        self.update_posture(obs, &los_mechs, near_mech_count, immediate_kill_avail, eye_h);

        let primary_mech = self.pick_best_mech_target(&mech_cands, me);
        let any_visible_mech = primary_mech.clone().or_else(|| {
            mech_cands
                .iter()
                .min_by(|a, b| a.dist.partial_cmp(&b.dist).unwrap())
                .cloned()
        });

        let combat_target = if self.posture == Posture::EmergencyRegen {
            primary_mech.clone()
        } else {
            primary_mech.clone().or_else(|| primary_farm.clone())
        };

        // Fire back while retreating in plain sight; hold fire once LOS broken or 1.8 s clear
        let actively_taking_fire = los_mech_count > 0
            && (t - self.last_damage_t < 1.8 || los_mechs.iter().any(|m| m.dist < 50.0));
        let hold_fire_for_regen = self.posture == Posture::EmergencyRegen
            && !immediate_kill_avail
            && !actively_taking_fire;

        let (fire, mut reload, chosen_aim, debug_aim_target) = if hold_fire_for_regen {
            ([false, false], false, None, None)
        } else if let Some(ref tgt) = combat_target {
            self.eval_combat(
                obs,
                muzzle,
                mech_radius,
                min_pitch,
                max_pitch,
                &mech_cands,
                tgt,
            )
        } else {
            let mut f = [false, false];
            for (idx, wopt) in me.weapons.iter().enumerate().take(2) {
                if let Some(w) = wopt {
                    if w.kind == WeaponKind::Laser && w.charge.unwrap_or(0.0) > 0.0 {
                        let charge = w.charge.unwrap_or(0.0);
                        let full_charge = stat(&w.stats, "charge", 0.75);
                        f[idx] = charge < full_charge;
                    }
                }
            }
            (f, false, None, None)
        };

        if fire[0] || fire[1] {
            self.last_combat_t = t;
        }

        // Proactive reload only when safely clear of enemies within 100 m
        let near_mech_100 = self
            .tracked_enemies
            .values()
            .filter(|e| dist2d(me.pos[0], me.pos[1], e.pos[0], e.pos[1]) <= 100.0)
            .count();
        if !reload && near_mech_100 == 0 {
            for wopt in me.weapons.iter().flatten() {
                if wopt.kind == WeaponKind::Gun && wopt.reload.unwrap_or(0.0) <= 0.0 {
                    let mag = stat(&wopt.stats, "mag", 12.0);
                    let ammo = wopt.ammo.unwrap_or(mag);
                    if ammo <= 3.0 {
                        reload = true;
                        break;
                    }
                }
            }
        }

        // Navigation, Cover & Context Steering
        let (goal_dir, goal_pt) = self.compute_nav_goal(
            obs,
            eye_h,
            &mech_cands,
            &los_mechs,
            primary_mech.as_ref().or(any_visible_mech.as_ref()),
            primary_farm.as_ref(),
        );

        let (move_dir, dodged_hazard, aimed_laser_dodge) =
            self.steer(obs, goal_dir, mech_radius, mech_height);
        self.last_move = move_dir;

        // Rear-armor shielding & active 360° scanning
        let final_aim = if let Some(aim) = chosen_aim {
            aim
        } else if t - self.last_damage_t <= 2.2 {
            if let Some(b) = self.last_damage_bearing {
                [b, 0.0]
            } else {
                [me.aim[0], 0.0]
            }
        } else if self.posture == Posture::EmergencyRegen {
            if let Some(closest_tracked) = self.closest_tracked_enemy_within(me.pos, 100.0) {
                let yaw = (closest_tracked.pos[1] - me.pos[1])
                    .atan2(closest_tracked.pos[0] - me.pos[0]);
                [yaw, 0.0]
            } else {
                [wrap_angle(me.aim[0] + 0.95 * self.last_scan_dir), 0.0]
            }
        } else if t - self.last_sound_t <= 0.8 && self.last_sound_is_near {
            if let Some(b) = self.last_sound_bearing {
                [b, 0.0]
            } else {
                [wrap_angle(me.aim[0] + 0.95 * self.last_scan_dir), 0.0]
            }
        } else {
            [wrap_angle(me.aim[0] + 0.95 * self.last_scan_dir), 0.0]
        };

        // Boost & jump (only jump when truly wedged for 5+ ticks; never on wall-graze)
        let moving = move_dir[0].hypot(move_dir[1]) > 0.25;
        let slot1_empty = me.weapons.get(1).and_then(|w| w.as_ref()).is_none();
        let stuck_jump = me.grounded && stuck_active;
        if stuck_jump {
            if t - self.last_strafe_flip_t > 0.6 {
                self.strafe_sign = -self.strafe_sign;
                self.last_strafe_flip_t = t;
            }
            self.cached_path.clear();
            self.unstuck_air_boost_until = t + 0.35;
        }

        let air_unstuck_boost = !me.grounded
            && me.vel[2] > 0.0
            && t <= self.unstuck_air_boost_until
            && self.cmd_move_streak >= 8
            && me.energy > 40.0;

        let ground_boost = me.grounded
            && moving
            && ((!me.in_zone && me.energy > 5.0)
                || (aimed_laser_dodge && me.energy > 25.0)
                || (dodged_hazard && me.energy > 15.0)
                || (self.posture == Posture::EmergencyRegen && los_mech_count > 0 && me.energy > 22.0)
                || (self.posture == Posture::ZoneRotate && me.energy > 25.0)
                || (self.posture == Posture::SupplyDrop && me.energy > 45.0)
                || (self.posture == Posture::LootWeapon && slot1_empty && me.energy > 35.0)
                || (self.posture == Posture::EarlyFarm && t < 35.0 && me.energy > 55.0));

        let boost = ground_boost || air_unstuck_boost;

        action.move_dir = Some(move_dir);
        action.aim = Some(final_aim);
        action.boost = Some(boost);
        action.jump = Some(stuck_jump);
        action.fire = Some(fire);
        if reload {
            action.reload = Some(true);
        }

        let wtag = weapon_loadout_tag(&me.weapons);
        let tgt_str = match combat_target.as_ref() {
            Some(tg) => format!("t={}", tg.id),
            None => "-".to_string(),
        };
        let mut label = format!(
            "{} hp={:.0} en={:.0} {} {}",
            self.posture.tag(),
            me.hp,
            me.energy,
            wtag,
            tgt_str
        );
        let max_chars = self.config.debug.max_text_chars.min(72);
        if label.len() > max_chars {
            label.truncate(max_chars);
        }
        let mut dbg = DebugDraw {
            label: Some(label),
            ..Default::default()
        };
        if self.config.debug.max_points >= 1 {
            dbg.points = Some(vec![DebugPoint {
                at: [goal_pt[0], goal_pt[1], me.pos[2] + 1.0],
                color: Some("#40c0ff"),
                size: Some(5.0),
            }]);
        }
        if let Some(tp) = debug_aim_target {
            if self.config.debug.max_lines >= 1 {
                dbg.lines = Some(vec![DebugLine {
                    a: muzzle,
                    b: tp,
                    color: Some("#ff5040"),
                }]);
            }
        }
        action.debug = Some(dbg);

        action
    }

    fn update_memory(&mut self, obs: &Observation) -> bool {
        let t = obs.t;
        let me = &obs.self_obs;
        let eye_h = stat(&me.stats, "eye_height", 8.0);
        let eye = [me.pos[0], me.pos[1], me.pos[2] + eye_h];

        self.blacklisted_pickups.retain(|_, exp_t| t - *exp_t <= 15.0);

        for ev in &obs.events {
            match ev {
                GameEvent::Damaged { bearing, by, .. } => {
                    self.last_combat_t = t;
                    if let Some(b) = bearing {
                        self.last_damage_bearing = Some(*b);
                        self.last_damage_t = t;
                        self.last_damage_by = *by;
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
                _ => {}
            }
        }

        // Track enemy mechs with vertical acceleration estimation
        for actor in &obs.mechs {
            if actor.kind != "mech" {
                continue;
            }
            let est_az = if let Some(prev) = self.tracked_enemies.get(&actor.id) {
                let dt = t - prev.last_seen_t;
                if (0.04..=0.5).contains(&dt) {
                    0.5 * prev.est_accel_z + 0.5 * ((actor.vel[2] - prev.vel[2]) / dt)
                } else {
                    0.0
                }
            } else {
                0.0
            };
            self.tracked_enemies.insert(
                actor.id,
                TrackedEnemy {
                    pos: actor.pos,
                    vel: actor.vel,
                    est_accel_z: est_az,
                    last_seen_t: t,
                },
            );
        }
        self.tracked_enemies.retain(|_, e| t - e.last_seen_t <= 4.0);

        // Remember reachable ground-level non-mortar weapon pickups spotted during scans
        let visible_pickup_ids: Vec<u64> = obs.pickups.iter().map(|p| p.id).collect();
        self.last_pickup_seen.retain(|id, (_, pos, seen_t)| {
            if self.blacklisted_pickups.contains_key(id) || t - *seen_t > 12.0 {
                return false;
            }
            let d = dist2d(me.pos[0], me.pos[1], pos[0], pos[1]);
            if !visible_pickup_ids.contains(id)
                && (d <= 6.0 || (d <= 35.0 && self.map.has_los(eye, [pos[0], pos[1], pos[2] + 0.5])))
            {
                return false;
            }
            true
        });
        for p in &obs.pickups {
            if p.kind == WeaponKind::Mortar || self.blacklisted_pickups.contains_key(&p.id) {
                continue;
            }
            if self.is_ground_level(p.pos) {
                self.last_pickup_seen.insert(p.id, (p.kind, p.pos, t));
            }
        }

        // Decay sound memory priority
        if t - self.last_sound_t > 2.5 {
            self.last_sound_priority = 0.0;
            self.last_sound_is_near = false;
        }
        for s in &obs.sounds {
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

        // Periodic strafe flip
        if t - self.last_strafe_flip_t > 2.4 {
            self.strafe_sign = -self.strafe_sign;
            self.last_strafe_flip_t = t;
        }

        stuck_active
    }

    fn pick_upgrade(
        &self,
        offer: &crate::protocol::Offer,
        me: &crate::protocol::SelfObs,
    ) -> usize {
        let mut best_idx = 0usize;
        let mut best_score = f64::NEG_INFINITY;

        for (i, choice) in offer.choices.iter().enumerate() {
            let score = if let Some(def) = self.config.upgrades.iter().find(|u| u.id == choice.id) {
                let held_req = def
                    .requires
                    .is_none_or(|req| me.weapons.iter().flatten().any(|w| w.kind == req));
                let existing_stacks = me.upgrades.iter().filter(|u| *u == &def.id).count();

                let mut max_eff = 10.0_f64;
                for eff in &def.effects {
                    let base = match eff.path.as_str() {
                        "mech.regen_per_s" | "mech.regen_delay" => 106.0,
                        "mech.armor" => 100.0,
                        "mech.hp" => 95.0,
                        "weapons.laser.damage" | "weapons.laser.charge" | "weapons.laser.pierce" => {
                            92.0
                        }
                        "weapons.lightning.damage"
                        | "weapons.lightning.energy"
                        | "weapons.lightning.range"
                        | "weapons.lightning.chain"
                        | "weapons.lightning.cone_deg" => 90.0,
                        "weapons.gun.cooldown" | "weapons.gun.damage" => 88.0,
                        "mech.near_range_bonus" | "mech.vision_bonus" | "mech.hearing" => {
                            if existing_stacks == 0 {
                                86.0
                            } else {
                                72.0
                            }
                        }
                        "weapons.gun.mag" | "weapons.gun.reload" => 80.0,
                        "mech.walk_speed" | "mech.boost_speed" => 78.0,
                        "mech.energy_regen" | "mech.boost_drain" | "mech.jet_drain" => 72.0,
                        "weapons.gun.speed" | "weapons.gun.range" => 70.0,
                        "mech.turn_rate" | "mech.pitch_rate" => 68.0,
                        "mech.energy" => 62.0,
                        p if p.starts_with("weapons.mortar.") => 35.0,
                        _ => 25.0,
                    };
                    let mag_bonus = if let Some(m) = eff.mul {
                        10.0 * (m - 1.0).abs().min(0.8)
                    } else if let Some(a) = eff.add {
                        if a.abs() > 1.0 { 3.0 } else { 4.0 * a.abs() }
                    } else {
                        0.0
                    };
                    max_eff = max_eff.max(base + mag_bonus);
                }
                let mut total = max_eff + 4.0 * (def.effects.len().saturating_sub(1) as f64);
                if !held_req {
                    total *= 0.25;
                }
                total
            } else {
                10.0
            };

            if score > best_score {
                best_score = score;
                best_idx = i;
            }
        }

        best_idx
    }

    fn pick_weapon_take_and_drop(
        &self,
        obs: &Observation,
        los_mechs: &[&TargetCandidate],
    ) -> (Option<u64>, Option<usize>) {
        let me = &obs.self_obs;
        let reach = (self.config.loot.pickup_radius - 0.1).max(0.5);
        let empty_slot = me.weapons.iter().take(2).position(|w| w.is_none());

        let mut best_pickup: Option<(u8, f64, WeaponKind, u64)> = None;
        for p in &obs.pickups {
            if p.kind == WeaponKind::Mortar || self.blacklisted_pickups.contains_key(&p.id) {
                continue;
            }
            if me.weapons.iter().flatten().any(|w| w.kind == p.kind) {
                continue;
            }
            let d2d = dist2d(me.pos[0], me.pos[1], p.pos[0], p.pos[1]);
            let dz = (me.pos[2] - p.pos[2]).abs();
            if d2d > reach || dz > 3.9 {
                continue;
            }
            let prio = weapon_desirability(p.kind);
            if best_pickup.is_none_or(|(bp, bd, _, _)| prio > bp || (prio == bp && d2d < bd)) {
                best_pickup = Some((prio, d2d, p.kind, p.id));
            }
        }

        let Some((prio, _, pk_kind, pid)) = best_pickup else {
            return (None, None);
        };

        if empty_slot.is_some() {
            return (Some(pid), None);
        }

        // Swapping when both slots full: never swap under close LOS fire, charging laser, or invested gun upgrades
        let close_threat = los_mechs.iter().any(|m| m.dist < 65.0);
        let laser_busy = me
            .weapons
            .iter()
            .flatten()
            .any(|w| w.kind == WeaponKind::Laser && w.charge.unwrap_or(0.0) > 0.0);
        if close_threat || laser_busy {
            return (None, None);
        }

        let has_gun_upgrades = me.upgrades.iter().any(|u| u.starts_with("gun_"));
        let holds_lightning = me
            .weapons
            .iter()
            .flatten()
            .any(|w| w.kind == WeaponKind::Lightning);

        let mut worst: Option<(u8, usize, WeaponKind)> = None;
        for (slot, wopt) in me.weapons.iter().enumerate().take(2) {
            if let Some(w) = wopt {
                let wp = weapon_desirability(w.kind);
                if worst.is_none_or(|(bw, _, _)| wp < bw) {
                    worst = Some((wp, slot, w.kind));
                }
            }
        }

        if let Some((worst_prio, worst_slot, worst_kind)) = worst {
            if prio > worst_prio {
                if worst_kind == WeaponKind::Gun {
                    if has_gun_upgrades {
                        return (None, None);
                    }
                    let allow_gun_swap =
                        obs.alive <= 10 || (holds_lightning && pk_kind == WeaponKind::Laser);
                    if !allow_gun_swap {
                        return (None, None);
                    }
                }
                return (Some(pid), Some(worst_slot));
            }
        }
        (None, None)
    }

    #[inline]
    fn is_ground_level(&self, pos: [f64; 3]) -> bool {
        (pos[2] - self.map.ground_height(pos[0], pos[1])).abs() <= 2.5
    }

    fn ensure_regen_cover(
        &mut self,
        obs: &Observation,
        eye_h: f64,
        los_mech_count: usize,
    ) -> Option<[f64; 2]> {
        let me = &obs.self_obs;
        let threats: Vec<[f64; 3]> = self
            .tracked_enemies
            .values()
            .filter(|e| dist2d(me.pos[0], me.pos[1], e.pos[0], e.pos[1]) <= 220.0)
            .map(|e| [e.pos[0], e.pos[1], e.pos[2] + 7.5])
            .collect();
        if threats.is_empty() {
            return None;
        }

        let arrived_exposed = self.cover_target.is_some_and(|cp| {
            dist2d(me.pos[0], me.pos[1], cp[0], cp[1]) <= 5.5 && los_mech_count > 0
        });
        if arrived_exposed {
            self.prev_cover_target = self.cover_target;
            self.cover_target = None;
        }

        let keep = self
            .cover_target
            .is_some_and(|_| obs.t - self.cover_target_t < 2.5);
        if !keep {
            self.cover_target = self.nav.find_cover_point_within(
                &self.map,
                me.pos,
                eye_h,
                &threats,
                obs.zone.center,
                obs.zone.radius,
                obs.zone.next_center,
                obs.zone.next_radius,
                48.0,
                self.prev_cover_target,
            );
            self.cover_target_t = obs.t;
        }
        self.cover_target
    }

    fn update_posture(
        &mut self,
        obs: &Observation,
        los_mechs: &[&TargetCandidate],
        near_mechs: usize,
        immediate_kill_avail: bool,
        eye_h: f64,
    ) {
        let me = &obs.self_obs;
        let los_count = los_mechs.len();
        let dist_next = dist2d(
            me.pos[0],
            me.pos[1],
            obs.zone.next_center[0],
            obs.zone.next_center[1],
        );
        let safe_r = (obs.zone.next_radius - 18.0).max(0.0);

        if !me.in_zone || (dist_next > safe_r && obs.t >= obs.zone.shrink_start - 20.0) {
            self.posture = Posture::ZoneRotate;
            return;
        }

        let want_regen = if self.posture == Posture::EmergencyRegen {
            !immediate_kill_avail
                && me.hp < 0.80 * me.max_hp
                && !(me.hp >= 0.72 * me.max_hp && near_mechs == 0)
        } else {
            !immediate_kill_avail
                && ((me.hp < 0.42 * me.max_hp && near_mechs > 0)
                    || (me.hp < 0.58 * me.max_hp && los_count >= 2))
        };
        if want_regen {
            if self.ensure_regen_cover(obs, eye_h, los_count).is_some()
                || (self.posture == Posture::EmergencyRegen && los_count == 0)
            {
                self.posture = Posture::EmergencyRegen;
                return;
            }
        }

        if self.find_best_supply_drop(obs, los_count).is_some() {
            self.posture = Posture::SupplyDrop;
            return;
        }

        if self.find_best_weapon_goal(obs, los_mechs).is_some() {
            self.posture = Posture::LootWeapon;
            return;
        }

        if self
            .tracked_enemies
            .values()
            .any(|e| dist2d(me.pos[0], me.pos[1], e.pos[0], e.pos[1]) <= 220.0)
        {
            self.posture = Posture::Combat;
            return;
        }

        self.posture = Posture::EarlyFarm;
    }

    fn select_targets(
        &self,
        obs: &Observation,
        muzzle: [f64; 3],
    ) -> (Vec<TargetCandidate>, Option<TargetCandidate>) {
        let me = &obs.self_obs;
        let base_mech_h = stat(&self.config.mech, "height", 9.0);
        let base_mech_r = stat(&self.config.mech, "radius", 2.5);
        let size_per_cube = self.config.cubes.size_per_cube;
        let npc_r = stat(&self.config.npc, "radius", 1.8);
        let npc_h = stat(&self.config.npc, "height", 3.5);
        let crate_r = stat(&self.config.crates, "radius", 1.4);
        let crate_h = stat(&self.config.crates, "height", 2.4);

        let mut mech_cands = Vec::new();
        let mut best_farm: Option<(f64, TargetCandidate)> = None;

        for actor in &obs.mechs {
            let d = dist3d(muzzle, actor.pos);
            if actor.kind == "mech" {
                let scale = 1.0 + size_per_cube * (actor.cubes.unwrap_or(0) as f64);
                let com_z = actor.pos[2] + 0.5 * base_mech_h * scale;
                let radius = base_mech_r * scale;
                let has_los = self
                    .map
                    .has_los(muzzle, [actor.pos[0], actor.pos[1], com_z]);
                let est_az = self
                    .tracked_enemies
                    .get(&actor.id)
                    .map(|e| e.est_accel_z)
                    .unwrap_or(0.0);
                mech_cands.push(TargetCandidate {
                    id: actor.id,
                    kind: TargetKind::Mech,
                    pos: actor.pos,
                    vel: actor.vel,
                    est_accel_z: est_az,
                    aim: actor.aim,
                    com_z,
                    radius,
                    hp: actor.hp.unwrap_or(1400.0),
                    max_hp: actor.max_hp.unwrap_or(1400.0),
                    weapons: actor.weapons.clone(),
                    charging: actor.charging,
                    has_los,
                    dist: d,
                });
            } else if actor.kind == "npc"
                && d <= 110.0
                && me.hp > 0.45 * me.max_hp
                && obs.t - self.last_damage_t > 2.5
            {
                let com_z = actor.pos[2] + 0.5 * npc_h;
                if self
                    .map
                    .has_los(muzzle, [actor.pos[0], actor.pos[1], com_z])
                {
                    let cand = TargetCandidate {
                        id: actor.id,
                        kind: TargetKind::Npc,
                        pos: actor.pos,
                        vel: actor.vel,
                        est_accel_z: 0.0,
                        aim: actor.aim,
                        com_z,
                        radius: npc_r,
                        hp: actor.hp.unwrap_or(350.0),
                        max_hp: actor.max_hp.unwrap_or(350.0),
                        weapons: Vec::new(),
                        charging: false,
                        has_los: true,
                        dist: d,
                    };
                    let score = 200.0 - d;
                    if best_farm.as_ref().is_none_or(|(bs, _)| score > *bs) {
                        best_farm = Some((score, cand));
                    }
                }
            }
        }

        if best_farm.is_none() && me.hp > 0.45 * me.max_hp && obs.t - self.last_damage_t > 2.5 {
            for c in &obs.crates {
                let d = dist3d(muzzle, c.pos);
                if d > 85.0 {
                    continue;
                }
                let com_z = c.pos[2] + 0.5 * crate_h;
                if self.map.has_los(muzzle, [c.pos[0], c.pos[1], com_z]) {
                    let cand = TargetCandidate {
                        id: c.id,
                        kind: TargetKind::Crate,
                        pos: c.pos,
                        vel: [0.0, 0.0, 0.0],
                        est_accel_z: 0.0,
                        aim: [0.0, 0.0],
                        com_z,
                        radius: crate_r,
                        hp: c.hp.unwrap_or(300.0),
                        max_hp: 300.0,
                        weapons: Vec::new(),
                        charging: false,
                        has_los: true,
                        dist: d,
                    };
                    let score = 100.0 - d;
                    if best_farm.as_ref().is_none_or(|(bs, _)| score > *bs) {
                        best_farm = Some((score, cand));
                    }
                }
            }
        }

        (mech_cands, best_farm.map(|(_, c)| c))
    }

    fn pick_best_mech_target(
        &self,
        cands: &[TargetCandidate],
        me: &crate::protocol::SelfObs,
    ) -> Option<TargetCandidate> {
        let have_laser = me
            .weapons
            .iter()
            .flatten()
            .any(|w| w.kind == WeaponKind::Laser);
        let lt_chain_range = me
            .weapons
            .iter()
            .flatten()
            .find(|w| w.kind == WeaponKind::Lightning)
            .map(|w| stat(&w.stats, "chain_range", 14.0));
        let have_lightning = lt_chain_range.is_some();

        let mut best_los: Option<(f64, &TargetCandidate)> = None;

        for c in cands {
            if !c.has_los {
                continue;
            }
            let mut score = 150.0 - c.dist;
            score += (c.max_hp - c.hp).max(0.0) * 0.22;
            if c.hp <= 360.0 {
                score += 120.0;
            }
            if have_laser && c.weapons.contains(&WeaponKind::Mortar) {
                score += 95.0; // Cook-off!
            }
            if have_lightning {
                if c.dist <= 50.0 && (c.charging || c.weapons.contains(&WeaponKind::Laser)) {
                    score += 110.0;
                }
                if let Some(cr) = lt_chain_range {
                    let cluster = cands
                        .iter()
                        .filter(|o| dist3d(c.pos, o.pos) <= cr)
                        .count();
                    if c.dist <= 48.0 && cluster >= 2 {
                        score += 45.0;
                    }
                }
            }
            if self.last_damage_by == Some(c.id) {
                score += 40.0;
            }

            if best_los.is_none_or(|(bs, _)| score > bs) {
                best_los = Some((score, c));
            }
        }

        best_los.map(|(_, c)| c.clone())
    }

    #[allow(clippy::too_many_arguments)]
    fn eval_combat(
        &self,
        obs: &Observation,
        muzzle: [f64; 3],
        mech_radius: f64,
        min_pitch: f64,
        max_pitch: f64,
        mech_cands: &[TargetCandidate],
        tgt: &TargetCandidate,
    ) -> ([bool; 2], bool, Option<[f64; 2]>, Option<[f64; 3]>) {
        let me = &obs.self_obs;
        let mut fire = [false, false];
        let mut reload = false;
        let chosen_aim: Option<[f64; 2]>;
        let debug_pt = Some([tgt.pos[0], tgt.pos[1], tgt.com_z]);

        let mut laser_slot = None;
        let mut gun_slot = None;
        let mut lightning_slot = None;

        for (i, wopt) in me.weapons.iter().enumerate().take(2) {
            if let Some(w) = wopt {
                match w.kind {
                    WeaponKind::Laser => laser_slot = Some(i),
                    WeaponKind::Gun => gun_slot = Some(i),
                    WeaponKind::Lightning => lightning_slot = Some(i),
                    WeaponKind::Mortar => {}
                }
            }
        }

        let active_laser = laser_slot.filter(|&i| {
            me.weapons[i]
                .as_ref()
                .is_some_and(|w| w.charge.unwrap_or(0.0) > 0.0)
        });

        let eval_gun_slot = |slot: usize| -> ([f64; 2], bool, bool, bool) {
            let w = me.weapons[slot].as_ref().unwrap();
            self.eval_gun(
                me.aim,
                muzzle,
                mech_radius,
                w,
                tgt,
                min_pitch,
                max_pitch,
            )
        };

        let eval_lt_slot = |slot: usize, aim_yaw: f64| -> ([f64; 2], bool) {
            let w = me.weapons[slot].as_ref().unwrap();
            let chain_r = stat(&w.stats, "chain_range", 14.0);
            let cluster_count = if matches!(tgt.kind, TargetKind::Mech) {
                mech_cands
                    .iter()
                    .filter(|o| dist3d(tgt.pos, o.pos) <= chain_r)
                    .count()
                    .max(1)
            } else {
                1
            };
            self.eval_lightning(
                me.aim,
                me.energy,
                muzzle,
                w,
                tgt,
                aim_yaw,
                cluster_count,
            )
        };

        // 1. Active charging laser takes priority for hold/release
        if let Some(ls) = active_laser {
            let w = me.weapons[ls].as_ref().unwrap();
            let full_charge = stat(&w.stats, "charge", 0.75);
            let charge = w.charge.unwrap_or(0.0);
            let (l_aim, should_hold) =
                self.eval_laser(me.aim, muzzle, me.cubes, w, tgt, min_pitch, max_pitch);
            fire[ls] = should_hold;

            if should_hold && charge < full_charge - 0.16 {
                if let Some(gs) = gun_slot {
                    let (g_aim, g_fire, g_rel, g_ready) = eval_gun_slot(gs);
                    if g_ready {
                        chosen_aim = Some(g_aim);
                        fire[gs] = g_fire;
                    } else {
                        chosen_aim = Some(l_aim);
                    }
                    reload = g_rel;
                } else {
                    chosen_aim = Some(l_aim);
                }
            } else {
                chosen_aim = Some(l_aim);
            }

            if let Some(lts) = lightning_slot {
                let aim_yaw = chosen_aim.map(|a| a[0]).unwrap_or(l_aim[0]);
                let (_, lt_fire) = eval_lt_slot(lts, aim_yaw);
                fire[lts] = lt_fire;
            }
            return (fire, reload, chosen_aim, debug_pt);
        }

        // 2. Lightning close-range check (tiered energy budget + simultaneous Gun pairing)
        if let Some(lts) = lightning_slot {
            let w = me.weapons[lts].as_ref().unwrap();
            let lt_range = stat(&w.stats, "range", 45.0);
            let chain_r = stat(&w.stats, "chain_range", 14.0);
            let e_cost = stat(&w.stats, "energy", 3.0);
            let cluster_count = if matches!(tgt.kind, TargetKind::Mech) {
                mech_cands
                    .iter()
                    .filter(|o| dist3d(tgt.pos, o.pos) <= chain_r)
                    .count()
                    .max(1)
            } else {
                1
            };
            let energy_ok = lightning_energy_ok(me.energy, e_cost, tgt, cluster_count);

            if tgt.has_los && tgt.dist <= lt_range - 0.5 && energy_ok {
                if let Some(gs) = gun_slot {
                    let (g_aim, g_fire, g_rel, g_ready) = eval_gun_slot(gs);
                    reload = g_rel;
                    if g_ready {
                        let (_, lt_fire) = eval_lt_slot(lts, g_aim[0]);
                        chosen_aim = Some(g_aim);
                        fire[gs] = g_fire;
                        fire[lts] = lt_fire;
                        return (fire, reload, chosen_aim, debug_pt);
                    }
                }
                let direct = direct_aim(muzzle, [tgt.pos[0], tgt.pos[1], tgt.com_z]);
                let (lt_aim, lt_fire) = eval_lt_slot(lts, direct[0]);
                chosen_aim = Some(lt_aim);
                fire[lts] = lt_fire;
                return (fire, reload, chosen_aim, debug_pt);
            }
        }

        // 3. Laser ready check
        if let Some(ls) = laser_slot {
            let w = me.weapons[ls].as_ref().unwrap();
            let l_range = stat(&w.stats, "range", 280.0);
            let allow_laser_farm =
                matches!(tgt.kind, TargetKind::Npc | TargetKind::Crate) && me.hp > 0.6 * me.max_hp;
            if tgt.has_los
                && tgt.dist <= l_range - 2.0
                && w.cooldown <= 0.08
                && (matches!(tgt.kind, TargetKind::Mech) || allow_laser_farm)
            {
                let (l_aim, should_hold) =
                    self.eval_laser(me.aim, muzzle, me.cubes, w, tgt, min_pitch, max_pitch);
                fire[ls] = should_hold;
                if let Some(gs) = gun_slot {
                    let (g_aim, g_fire, g_rel, g_ready) = eval_gun_slot(gs);
                    reload = g_rel;
                    if g_ready {
                        chosen_aim = Some(g_aim);
                        fire[gs] = g_fire;
                    } else {
                        chosen_aim = Some(l_aim);
                    }
                } else {
                    chosen_aim = Some(l_aim);
                }
                return (fire, reload, chosen_aim, debug_pt);
            }
        }

        // 4. Gun check
        if let Some(gs) = gun_slot {
            let w = me.weapons[gs].as_ref().unwrap();
            let g_range = stat(&w.stats, "range", 230.0);
            if tgt.has_los && tgt.dist <= g_range {
                let (g_aim, g_fire, g_rel, _) = eval_gun_slot(gs);
                chosen_aim = Some(g_aim);
                fire[gs] = g_fire;
                reload = g_rel;
                return (fire, reload, chosen_aim, debug_pt);
            }
        }

        chosen_aim = Some(direct_aim(muzzle, [tgt.pos[0], tgt.pos[1], tgt.com_z]));
        (fire, reload, chosen_aim, debug_pt)
    }

    #[allow(clippy::too_many_arguments)]
    fn eval_gun(
        &self,
        cur_aim: [f64; 2],
        muzzle: [f64; 3],
        mech_radius: f64,
        w: &WeaponObs,
        tgt: &TargetCandidate,
        min_pitch: f64,
        max_pitch: f64,
    ) -> ([f64; 2], bool, bool, bool) {
        let speed = stat(&w.stats, "speed", 252.0);
        let range = stat(&w.stats, "range", 230.0);
        let mag = stat(&w.stats, "mag", 12.0);
        let ammo = w.ammo.unwrap_or(mag);
        let reload_left = w.reload.unwrap_or(0.0);
        let gravity = stat(&self.config.mech, "gravity", 22.0);

        let (aim, lead_pos) = solve_gun_lead_airborne(
            &self.map,
            muzzle,
            tgt.pos,
            tgt.vel,
            tgt.est_accel_z,
            tgt.com_z - tgt.pos[2],
            gravity,
            speed,
            mech_radius + 0.5,
            min_pitch,
            max_pitch,
        );

        let need_reload = reload_left <= 0.0 && ammo <= 0.0;
        let dist = dist3d(muzzle, lead_pos);
        // Never gate trigger on cooldown <= 0.12: holding trigger fires cleanly every 0.24 s!
        let ready = reload_left <= 0.0 && ammo > 0.0 && dist <= range;
        let half_h = (tgt.com_z - tgt.pos[2]).max(1.2);
        let slew_ok = slew_aligned(
            cur_aim,
            aim,
            tgt.radius,
            half_h,
            dist.max(4.0),
            1.25,
            0.012,
        );
        let los_ok = tgt.has_los && self.map.has_los(muzzle, lead_pos);

        (aim, ready && slew_ok && los_ok, need_reload, ready)
    }

    #[allow(clippy::too_many_arguments)]
    fn eval_lightning(
        &self,
        cur_aim: [f64; 2],
        energy: f64,
        muzzle: [f64; 3],
        w: &WeaponObs,
        tgt: &TargetCandidate,
        preferred_yaw: f64,
        cluster_count: usize,
    ) -> ([f64; 2], bool) {
        let range = stat(&w.stats, "range", 45.0);
        let cone_rad = stat(&w.stats, "cone_deg", 30.0).to_radians();
        let e_cost = stat(&w.stats, "energy", 3.0);

        let direct = direct_aim(muzzle, [tgt.pos[0], tgt.pos[1], tgt.com_z]);
        let check_yaw = if angle_diff(preferred_yaw, direct[0]).abs() < 0.35 * cone_rad {
            direct[0]
        } else {
            preferred_yaw
        };
        let yaw_err = angle_diff(cur_aim[0], check_yaw).abs();
        let energy_ok = lightning_energy_ok(energy, e_cost, tgt, cluster_count);
        let should_fire =
            tgt.has_los && tgt.dist <= range - 0.5 && yaw_err <= 0.5 * cone_rad && energy_ok;
        (direct, should_fire)
    }

    #[allow(clippy::too_many_arguments)]
    fn eval_laser(
        &self,
        cur_aim: [f64; 2],
        muzzle: [f64; 3],
        my_cubes: u64,
        w: &WeaponObs,
        tgt: &TargetCandidate,
        min_pitch: f64,
        max_pitch: f64,
    ) -> ([f64; 2], bool) {
        let com_off = tgt.com_z - tgt.pos[2];
        let hit_x = tgt.pos[0] + tgt.vel[0] * 0.04;
        let hit_y = tgt.pos[1] + tgt.vel[1] * 0.04;
        let surf_hit = self.map.surface_height(hit_x, hit_y, tgt.pos[2]) + com_off;
        let hit_z = (tgt.com_z + tgt.vel[2] * 0.04).max(surf_hit);
        let hit_pt = [hit_x, hit_y, hit_z];
        let mut aim = direct_aim(muzzle, hit_pt);
        aim[1] = aim[1].clamp(min_pitch, max_pitch);

        let full_charge = stat(&w.stats, "charge", 0.75);
        let min_charge = stat(&w.stats, "min_charge", 0.15);
        let hold_s = stat(&w.stats, "hold_s", 2.0);
        let range = stat(&w.stats, "range", 280.0);
        let damage = stat(&w.stats, "damage", 315.0);
        let min_frac = stat(&w.stats, "min_frac", 0.35);
        let cookoff = stat(&w.stats, "cookoff", 0.6);
        let charge = w.charge.unwrap_or(0.0);

        let half_h = com_off.max(1.2);
        let aligned = slew_aligned(
            cur_aim,
            aim,
            tgt.radius,
            half_h,
            tgt.dist.max(5.0),
            0.95,
            0.0,
        );

        let frac = min_frac + (1.0 - min_frac) * (charge / full_charge.max(0.01)).clamp(0.0, 1.0);
        let cube_mult = 1.0
            + self.config.cubes.damage_per_cube
                * (my_cubes as f64).min(self.config.cubes.stat_cap);
        let cookoff_mult = if tgt.weapons.contains(&WeaponKind::Mortar) {
            1.0 + cookoff
        } else {
            1.0
        };
        let front_half = 0.5 * stat(&self.config.mech, "front_arc_deg", 180.0).to_radians();
        let rear_mult_cfg = stat(&self.config.mech, "rear_mult", 1.5);
        let dir_to_me = (muzzle[1] - tgt.pos[1]).atan2(muzzle[0] - tgt.pos[0]);
        let rear_mult = if matches!(tgt.kind, TargetKind::Mech)
            && angle_diff(tgt.aim[0], dir_to_me).abs() > front_half
        {
            rear_mult_cfg
        } else {
            1.0
        };
        let expected_dmg = damage * frac * cube_mult * cookoff_mult * rear_mult;

        let future_pt = [
            tgt.pos[0] + tgt.vel[0] * 0.25,
            tgt.pos[1] + tgt.vel[1] * 0.25,
            tgt.com_z,
        ];
        let losing_los = tgt.has_los && !self.map.has_los(muzzle, future_pt);

        let should_hold = if charge <= 0.0 {
            tgt.has_los && tgt.dist <= range - 2.0 && w.cooldown <= 0.08
        } else if charge >= full_charge + hold_s - 0.25 {
            false
        } else if !tgt.has_los && charge >= full_charge + 0.6 {
            false
        } else if charge >= full_charge * 0.90 && tgt.has_los && aligned {
            false
        } else if charge >= min_charge
            && tgt.has_los
            && aligned
            && (expected_dmg >= tgt.hp * 1.05 || losing_los)
        {
            false
        } else {
            true
        };

        (aim, should_hold)
    }

    fn compute_nav_goal(
        &mut self,
        obs: &Observation,
        eye_h: f64,
        mech_cands: &[TargetCandidate],
        los_mechs: &[&TargetCandidate],
        combat_enemy: Option<&TargetCandidate>,
        farm_target: Option<&TargetCandidate>,
    ) -> ([f64; 2], [f64; 2]) {
        let me = &obs.self_obs;
        let los_mech_count = los_mechs.len();
        let me_xy = [me.pos[0], me.pos[1]];
        let nc = obs.zone.next_center;
        let safe_r = (obs.zone.next_radius - 18.0).max(0.0);
        let dist_next = dist2d(me.pos[0], me.pos[1], nc[0], nc[1]);

        match self.posture {
            Posture::ZoneRotate => {
                let wp = self.follow_astar(me_xy, nc, obs.t);
                return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), nc);
            }
            Posture::EmergencyRegen => {
                if let Some(cp) = self.ensure_regen_cover(obs, eye_h, los_mech_count) {
                    if dist2d(me.pos[0], me.pos[1], cp[0], cp[1]) <= 5.0 && los_mech_count == 0 {
                        return ([0.0, 0.0], cp);
                    }
                    let wp = self.follow_astar(me_xy, cp, obs.t);
                    return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), cp);
                }
                if let Some(th) = self.closest_tracked_enemy_within(me.pos, 220.0) {
                    let away = unit2d(me.pos[0] - th.pos[0], me.pos[1] - th.pos[1]);
                    let zdir = unit2d(nc[0] - me.pos[0], nc[1] - me.pos[1]);
                    let goal = [
                        me.pos[0] + (away[0] * 0.75 + zdir[0] * 0.35) * 40.0,
                        me.pos[1] + (away[1] * 0.75 + zdir[1] * 0.35) * 40.0,
                    ];
                    let wp = self.follow_astar(me_xy, goal, obs.t);
                    return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), goal);
                }
                return (unit2d(nc[0] - me.pos[0], nc[1] - me.pos[1]), nc);
            }
            Posture::SupplyDrop => {
                if let Some(drop) = self.find_best_supply_drop(obs, los_mech_count) {
                    let d = dist2d(me.pos[0], me.pos[1], drop.pos[0], drop.pos[1]);
                    if drop.lands_in > 5.0 && d < 28.0 {
                        let threats: Vec<[f64; 3]> = mech_cands
                            .iter()
                            .filter(|m| m.has_los)
                            .map(|m| [m.pos[0], m.pos[1], m.pos[2] + 7.5])
                            .collect();
                        if !threats.is_empty() {
                            if let Some(cp) = self.nav.find_cover_point(
                                &self.map,
                                me.pos,
                                eye_h,
                                &threats,
                                obs.zone.center,
                                obs.zone.radius,
                                obs.zone.next_center,
                                obs.zone.next_radius,
                            ) {
                                let wp = self.follow_astar(me_xy, cp, obs.t);
                                return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), cp);
                            }
                        }
                        let ang = obs.t * 1.1 * self.strafe_sign;
                        let orbit = [
                            drop.pos[0] + 16.0 * ang.cos(),
                            drop.pos[1] + 16.0 * ang.sin(),
                        ];
                        return (unit2d(orbit[0] - me.pos[0], orbit[1] - me.pos[1]), orbit);
                    }
                    if drop.lands_in <= 4.5 && d <= (drop.radius - 1.5).max(1.0) {
                        let r_in = (drop.radius * 0.55).max(1.5);
                        let ang = obs.t * 1.8 * self.strafe_sign;
                        let pt = [
                            drop.pos[0] + r_in * ang.cos(),
                            drop.pos[1] + r_in * ang.sin(),
                        ];
                        return (unit2d(pt[0] - me.pos[0], pt[1] - me.pos[1]), pt);
                    }
                    let wp = self.follow_astar(me_xy, [drop.pos[0], drop.pos[1]], obs.t);
                    return (
                        unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]),
                        [drop.pos[0], drop.pos[1]],
                    );
                }
            }
            Posture::LootWeapon => {
                if let Some(pk_pos) = self.find_best_weapon_goal(obs, los_mechs) {
                    let wp = self.follow_astar(me_xy, pk_pos, obs.t);
                    return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), pk_pos);
                }
            }
            Posture::Combat => {
                let no_weapon_can_shoot_now = if let Some(tgt) = combat_enemy {
                    let any_charging = me
                        .weapons
                        .iter()
                        .flatten()
                        .any(|w| w.kind == WeaponKind::Laser && w.charge.unwrap_or(0.0) > 0.0);
                    let any_ready = me.weapons.iter().flatten().any(|w| match w.kind {
                        WeaponKind::Gun => {
                            let r = stat(&w.stats, "range", 230.0);
                            w.cooldown <= 0.38
                                && w.reload.unwrap_or(0.0) <= 0.0
                                && w.ammo.unwrap_or(1.0) > 0.0
                                && tgt.dist <= r
                        }
                        WeaponKind::Laser => {
                            let r = stat(&w.stats, "range", 280.0);
                            w.cooldown <= 0.38 && tgt.dist <= r
                        }
                        WeaponKind::Lightning => {
                            let r = stat(&w.stats, "range", 45.0);
                            let e_cost = stat(&w.stats, "energy", 3.0);
                            w.cooldown <= 0.38 && tgt.dist <= r && me.energy >= 10.0_f64.max(e_cost)
                        }
                        WeaponKind::Mortar => false,
                    });
                    !any_charging && !any_ready
                } else {
                    false
                };

                let have_laser = me
                    .weapons
                    .iter()
                    .flatten()
                    .any(|w| w.kind == WeaponKind::Laser);
                let anti_laser_duck = !have_laser
                    && combat_enemy.is_some_and(|e| {
                        e.weapons.contains(&WeaponKind::Laser) && e.charging && e.dist > 50.0
                    });

                if los_mech_count >= 2 || no_weapon_can_shoot_now || anti_laser_duck {
                    let threats: Vec<[f64; 3]> = mech_cands
                        .iter()
                        .filter(|m| m.has_los)
                        .map(|m| [m.pos[0], m.pos[1], m.pos[2] + 7.5])
                        .collect();
                    if !threats.is_empty() {
                        if let Some(cp) = self.nav.find_cover_point_within(
                            &self.map,
                            me.pos,
                            eye_h,
                            &threats,
                            obs.zone.center,
                            obs.zone.radius,
                            obs.zone.next_center,
                            obs.zone.next_radius,
                            48.0,
                            None,
                        ) {
                            let wp = self.follow_astar(me_xy, cp, obs.t);
                            return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), cp);
                        }
                    }
                }

                if let Some(e) = combat_enemy {
                    if !e.has_los && e.dist > 35.0 {
                        let goal = [e.pos[0], e.pos[1]];
                        let wp = self.follow_astar(me_xy, goal, obs.t);
                        return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), goal);
                    }
                    let to_e = unit2d(e.pos[0] - me.pos[0], e.pos[1] - me.pos[1]);
                    let perp = [-to_e[1] * self.strafe_sign, to_e[0] * self.strafe_sign];
                    let has_lightning = me
                        .weapons
                        .iter()
                        .flatten()
                        .any(|w| w.kind == WeaponKind::Lightning);
                    let enemy_has_laser = e.weapons.contains(&WeaponKind::Laser);
                    let pref_dist =
                        if has_lightning && me.energy > 30.0 && (e.charging || e.dist < 52.0) {
                            30.0
                        } else if enemy_has_laser && !have_laser {
                            if has_lightning { 30.0 } else { 45.0 }
                        } else if me.hp < 0.50 * me.max_hp {
                            92.0
                        } else {
                            64.0
                        };
                    let d2 = dist2d(me.pos[0], me.pos[1], e.pos[0], e.pos[1]);
                    let radial_w = if d2 < pref_dist - 12.0 {
                        -0.65
                    } else if d2 > pref_dist + 18.0 {
                        0.55
                    } else {
                        0.0
                    };
                    let mut gx = to_e[0] * radial_w + perp[0] * 0.85;
                    let mut gy = to_e[1] * radial_w + perp[1] * 0.85;
                    if dist_next > safe_r * 0.85 {
                        let zdir = unit2d(nc[0] - me.pos[0], nc[1] - me.pos[1]);
                        gx += zdir[0] * 0.55;
                        gy += zdir[1] * 0.55;
                    }
                    let dir = unit2d(gx, gy);
                    let goal_pt = [me.pos[0] + dir[0] * 20.0, me.pos[1] + dir[1] * 20.0];
                    return (dir, goal_pt);
                }
            }
            Posture::EarlyFarm => {}
        }

        // EarlyFarm / fallback: ground cubes -> NPCs -> ground crates -> safe interior
        if let Some(cube) = obs
            .cubes
            .iter()
            .filter(|c| {
                self.is_ground_level(c.pos)
                    && dist2d(me.pos[0], me.pos[1], c.pos[0], c.pos[1]) <= 65.0
                    && dist2d(c.pos[0], c.pos[1], nc[0], nc[1]) <= obs.zone.next_radius - 5.0
            })
            .min_by(|a, b| {
                dist2d(me.pos[0], me.pos[1], a.pos[0], a.pos[1])
                    .partial_cmp(&dist2d(me.pos[0], me.pos[1], b.pos[0], b.pos[1]))
                    .unwrap()
            })
        {
            let goal = [cube.pos[0], cube.pos[1]];
            let wp = self.follow_astar(me_xy, goal, obs.t);
            return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), goal);
        }

        if let Some(ft) = farm_target {
            let goal = [ft.pos[0], ft.pos[1]];
            let d = dist2d(me.pos[0], me.pos[1], goal[0], goal[1]);
            let stop_d = if matches!(ft.kind, TargetKind::Npc) {
                26.0
            } else {
                18.0
            };
            if d > stop_d {
                let wp = self.follow_astar(me_xy, goal, obs.t);
                return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), goal);
            }
        }

        if me.hp > 0.45 * me.max_hp && obs.t - self.last_damage_t > 2.5 {
            if let Some(cr) = obs
                .crates
                .iter()
                .filter(|c| {
                    self.is_ground_level(c.pos)
                        && dist2d(me.pos[0], me.pos[1], c.pos[0], c.pos[1]) <= 85.0
                        && dist2d(c.pos[0], c.pos[1], nc[0], nc[1]) <= obs.zone.next_radius - 5.0
                })
                .min_by(|a, b| {
                    dist2d(me.pos[0], me.pos[1], a.pos[0], a.pos[1])
                        .partial_cmp(&dist2d(me.pos[0], me.pos[1], b.pos[0], b.pos[1]))
                        .unwrap()
                })
            {
                let goal = [cr.pos[0], cr.pos[1]];
                if dist2d(me.pos[0], me.pos[1], goal[0], goal[1]) > 18.0 {
                    let wp = self.follow_astar(me_xy, goal, obs.t);
                    return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), goal);
                }
            }
        }

        if dist_next > safe_r * 0.65 {
            let wp = self.follow_astar(me_xy, nc, obs.t);
            return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), nc);
        }

        let ang = (obs.t * 0.25) + (self.strafe_sign * 0.5);
        let patrol_r = (safe_r * 0.40).min(110.0);
        let patrol_pt = [nc[0] + patrol_r * ang.cos(), nc[1] + patrol_r * ang.sin()];
        let wp = self.follow_astar(me_xy, patrol_pt, obs.t);
        (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), patrol_pt)
    }

    fn follow_astar(&mut self, start: [f64; 2], goal: [f64; 2], t: f64) -> [f64; 2] {
        let d_direct = dist2d(start[0], start[1], goal[0], goal[1]);
        if d_direct <= 14.0 {
            return goal;
        }
        let goal_moved = dist2d(
            goal[0],
            goal[1],
            self.cached_path_goal[0],
            self.cached_path_goal[1],
        ) > 10.0;
        if self.cached_path.is_empty() || goal_moved || t - self.cached_path_t >= 0.4 {
            self.cached_path =
                self.nav
                    .find_path(start, goal, 2400, &mut self.nav_scratch);
            self.cached_path_goal = goal;
            self.cached_path_t = t;
        }

        while self.cached_path.len() > 1
            && dist2d(
                start[0],
                start[1],
                self.cached_path[0][0],
                self.cached_path[0][1],
            ) <= 6.5
        {
            self.cached_path.remove(0);
        }

        self.cached_path.first().copied().unwrap_or(goal)
    }

    fn find_best_supply_drop<'a>(
        &self,
        obs: &'a Observation,
        los_mechs: usize,
    ) -> Option<&'a DropObs> {
        let me = &obs.self_obs;
        let max_pull = if me.hp < 0.65 * me.max_hp {
            320.0
        } else {
            220.0
        };
        let mut best: Option<(f64, &'a DropObs)> = None;
        for d in &obs.drops {
            if dist2d(d.pos[0], d.pos[1], obs.zone.center[0], obs.zone.center[1])
                > obs.zone.radius - 10.0
            {
                continue;
            }
            let campers = self
                .tracked_enemies
                .values()
                .filter(|e| dist2d(e.pos[0], e.pos[1], d.pos[0], d.pos[1]) <= 25.0)
                .count();
            if campers >= 2 {
                continue;
            }
            let dist = dist2d(me.pos[0], me.pos[1], d.pos[0], d.pos[1]);
            if d.lands_in <= 18.0
                && dist <= max_pull
                && (los_mechs <= 1 || dist <= d.radius + 15.0)
                && best.is_none_or(|(bd, _)| dist < bd)
            {
                best = Some((dist, d));
            }
        }
        best.map(|(_, d)| d)
    }

    fn find_best_weapon_goal(
        &self,
        obs: &Observation,
        los_mechs: &[&TargetCandidate],
    ) -> Option<[f64; 2]> {
        let me = &obs.self_obs;
        let slot1_empty = me.weapons.get(1).and_then(|w| w.as_ref()).is_none();
        let has_gun_upgrades = me.upgrades.iter().any(|u| u.starts_with("gun_"));
        let holds_lightning = me
            .weapons
            .iter()
            .flatten()
            .any(|w| w.kind == WeaponKind::Lightning);

        let worst_held = me
            .weapons
            .iter()
            .take(2)
            .flatten()
            .map(|w| (weapon_desirability(w.kind), w.kind))
            .min_by_key(|&(p, _)| p);

        let close_los_enemy = los_mechs.iter().any(|m| m.dist < 60.0);
        let any_los_enemy = !los_mechs.is_empty();
        let max_seek_d = if slot1_empty { 185.0 } else { 65.0 };
        let mut best: Option<(f64, [f64; 2])> = None;

        for (id, &(kind, pos, _)) in &self.last_pickup_seen {
            if kind == WeaponKind::Mortar || self.blacklisted_pickups.contains_key(id) {
                continue;
            }
            if me.weapons.iter().flatten().any(|w| w.kind == kind) {
                continue;
            }
            if !self.is_ground_level(pos) {
                continue;
            }
            if dist2d(pos[0], pos[1], obs.zone.next_center[0], obs.zone.next_center[1])
                > obs.zone.next_radius - 6.0
            {
                continue;
            }
            let d = dist2d(me.pos[0], me.pos[1], pos[0], pos[1]);
            if d > max_seek_d {
                continue;
            }

            let prio = weapon_desirability(kind);
            if slot1_empty {
                if kind == WeaponKind::Gun || (close_los_enemy && d >= 35.0) {
                    continue;
                }
            } else {
                let Some((worst_prio, worst_kind)) = worst_held else {
                    continue;
                };
                if prio <= worst_prio || any_los_enemy {
                    continue;
                }
                if worst_kind == WeaponKind::Gun {
                    if has_gun_upgrades {
                        continue;
                    }
                    let allow_gun_swap =
                        obs.alive <= 10 || (holds_lightning && kind == WeaponKind::Laser);
                    if !allow_gun_swap {
                        continue;
                    }
                }
            }

            let score = prio as f64 * 45.0 - d;
            if best.is_none_or(|(bs, _)| score > bs) {
                best = Some((score, [pos[0], pos[1]]));
            }
        }

        best.map(|(_, pt)| pt)
    }

    fn closest_tracked_enemy_within(&self, me_pos: [f64; 3], max_d: f64) -> Option<&TrackedEnemy> {
        self.tracked_enemies
            .values()
            .filter(|e| dist2d(me_pos[0], me_pos[1], e.pos[0], e.pos[1]) <= max_d)
            .min_by(|a, b| {
                dist2d(me_pos[0], me_pos[1], a.pos[0], a.pos[1])
                    .partial_cmp(&dist2d(me_pos[0], me_pos[1], b.pos[0], b.pos[1]))
                    .unwrap()
            })
    }

    fn steer(
        &self,
        obs: &Observation,
        goal_dir: [f64; 2],
        mech_radius: f64,
        mech_height: f64,
    ) -> ([f64; 2], bool, bool) {
        let me = &obs.self_obs;
        let step_up = stat(&me.stats, "step_up", 1.2);
        let max_slope_rad = stat(&me.stats, "max_slope_deg", 50.0).to_radians();
        let max_slope_tan = max_slope_rad.tan();
        let mortar_g = stat(&self.config.weapons.mortar, "gravity", 22.0);
        let splash_r = stat(&self.config.weapons.mortar, "splash_radius", 7.0);

        let mut mortar_threats: Vec<[f64; 2]> = Vec::new();
        for proj in &obs.projectiles {
            if proj.kind != "mortar" {
                continue;
            }
            if let Some(impact) = self.predict_mortar_impact(proj.pos, proj.vel, mortar_g) {
                if dist2d(me.pos[0], me.pos[1], impact[0], impact[1]) <= splash_r + 10.0 {
                    mortar_threats.push(impact);
                }
            }
        }

        // Exact ray-distance anti-laser dodge: ray from charging enemy within 8 m perpendicular distance
        let mut laser_threats: Vec<[f64; 2]> = Vec::new();
        for m in &obs.mechs {
            if m.kind == "mech" && m.charging {
                let ray = [m.aim[0].cos(), m.aim[0].sin()];
                let rel = [me.pos[0] - m.pos[0], me.pos[1] - m.pos[1]];
                let proj = rel[0] * ray[0] + rel[1] * ray[1];
                let perp_dist = (rel[0] * ray[1] - rel[1] * ray[0]).abs();
                if proj > 0.0 && proj <= 265.0 && perp_dist <= 8.0 {
                    laser_threats.push(ray);
                }
            }
        }

        let aimed_laser_dodge = !laser_threats.is_empty();
        let dodged_hazard = !mortar_threats.is_empty() || aimed_laser_dodge;

        if goal_dir[0].hypot(goal_dir[1]) < 0.05 && !dodged_hazard {
            return ([0.0, 0.0], false, false);
        }

        let on_ground_here =
            me.grounded && (me.pos[2] - self.map.ground_height(me.pos[0], me.pos[1])).abs() <= 2.0;

        let mut best_dir = goal_dir;
        let mut best_score = f64::NEG_INFINITY;

        for k in 0..16 {
            let theta = (k as f64) * (PI / 8.0);
            let dir = [theta.cos(), theta.sin()];
            let mut score = dir[0] * goal_dir[0] + dir[1] * goal_dir[1];
            score += 0.18 * (dir[0] * self.last_move[0] + dir[1] * self.last_move[1]);

            let check_step = |dist_m: f64| -> bool {
                let nx = me.pos[0] + dir[0] * dist_m;
                let ny = me.pos[1] + dir[1] * dist_m;
                let g_next = self.map.ground_height(nx, ny);
                let dz_g = g_next - me.pos[2];
                if on_ground_here && dz_g > step_up && dz_g / dist_m > max_slope_tan {
                    return false;
                }
                let probe_z = if on_ground_here { g_next } else { me.pos[2] };
                let (ok, surf_z) = self.map.is_pos_walkable(
                    nx,
                    ny,
                    probe_z,
                    mech_radius,
                    mech_height,
                    step_up,
                );
                if !ok {
                    return false;
                }
                let dz = surf_z - me.pos[2];
                if dz > step_up && dz / dist_m > max_slope_tan {
                    return false;
                }
                true
            };

            let ok_near = check_step(3.5);
            let ok_mid = ok_near && check_step(7.5);
            let ok_far = ok_mid && check_step(13.0);

            if !ok_near {
                score -= 4.0;
            } else if !ok_mid {
                score -= 1.8;
            } else if !ok_far {
                score -= 0.6;
            }

            for imp in &mortar_threats {
                let away = unit2d(me.pos[0] - imp[0], me.pos[1] - imp[1]);
                score += 2.2 * (dir[0] * away[0] + dir[1] * away[1]);
            }

            for ray in &laser_threats {
                let cross = (dir[0] * ray[1] - dir[1] * ray[0]).abs();
                score += 3.5 * cross;
            }

            if score > best_score {
                best_score = score;
                best_dir = dir;
            }
        }

        (best_dir, dodged_hazard, aimed_laser_dodge)
    }

    fn predict_mortar_impact(&self, pos: [f64; 3], vel: [f64; 3], g: f64) -> Option<[f64; 2]> {
        let mut t = 0.2;
        for _ in 0..14 {
            let x = pos[0] + vel[0] * t;
            let y = pos[1] + vel[1] * t;
            let z = pos[2] + vel[2] * t - 0.5 * g * t * t;
            let surf = self.map.surface_height(x, y, z + 5.0);
            if z <= surf {
                return Some([x, y]);
            }
            t += 0.25;
        }
        None
    }
}

#[inline]
fn lightning_energy_ok(
    energy: f64,
    e_cost: f64,
    tgt: &TargetCandidate,
    cluster_count: usize,
) -> bool {
    match tgt.kind {
        TargetKind::Crate | TargetKind::Npc => energy >= 30.0_f64.max(e_cost),
        TargetKind::Mech => {
            if tgt.charging || tgt.hp < 320.0 || cluster_count >= 2 {
                energy >= 10.0_f64.max(e_cost)
            } else {
                energy >= 25.0_f64.max(e_cost)
            }
        }
    }
}

#[inline]
fn weapon_desirability(kind: WeaponKind) -> u8 {
    match kind {
        WeaponKind::Laser => 4,
        WeaponKind::Lightning => 3,
        WeaponKind::Gun => 2,
        WeaponKind::Mortar => 0,
    }
}

fn weapon_loadout_tag(weapons: &[Option<WeaponObs>]) -> String {
    let mut s = String::with_capacity(2);
    for wopt in weapons.iter().take(2) {
        let ch = match wopt.as_ref().map(|w| w.kind) {
            Some(WeaponKind::Gun) => 'G',
            Some(WeaponKind::Laser) => 'L',
            Some(WeaponKind::Lightning) => 'Z',
            Some(WeaponKind::Mortar) => 'M',
            None => '-',
        };
        s.push(ch);
    }
    s
}

#[inline]
fn dist2d(x0: f64, y0: f64, x1: f64, y1: f64) -> f64 {
    (x1 - x0).hypot(y1 - y0)
}

#[inline]
fn dist3d(a: [f64; 3], b: [f64; 3]) -> f64 {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let dz = b[2] - a[2];
    (dx * dx + dy * dy + dz * dz).sqrt()
}

#[inline]
fn unit2d(x: f64, y: f64) -> [f64; 2] {
    let len = x.hypot(y);
    if len > 1e-6 {
        [x / len, y / len]
    } else {
        [0.0, 0.0]
    }
}

#[inline]
fn wrap_angle(mut a: f64) -> f64 {
    while a > PI {
        a -= 2.0 * PI;
    }
    while a < -PI {
        a += 2.0 * PI;
    }
    a
}

#[inline]
fn angle_diff(from: f64, to: f64) -> f64 {
    wrap_angle(to - from)
}

#[inline]
fn direct_aim(from: [f64; 3], to: [f64; 3]) -> [f64; 2] {
    let dx = to[0] - from[0];
    let dy = to[1] - from[1];
    let dz = to[2] - from[2];
    let horiz = dx.hypot(dy).max(1e-6);
    [dy.atan2(dx), dz.atan2(horiz)]
}

#[allow(clippy::too_many_arguments)]
pub fn solve_gun_lead_airborne(
    map: &Map,
    muzzle: [f64; 3],
    target_feet: [f64; 3],
    target_vel: [f64; 3],
    est_accel_z: f64,
    com_offset_z: f64,
    gravity: f64,
    bullet_speed: f64,
    shot_origin_offset: f64,
    min_pitch: f64,
    max_pitch: f64,
) -> ([f64; 2], [f64; 3]) {
    let target_com = [target_feet[0], target_feet[1], target_feet[2] + com_offset_z];
    let ground_here = map.surface_height(target_feet[0], target_feet[1], target_feet[2]);
    let is_airborne = (target_feet[2] - ground_here).abs() > 0.35 || target_vel[2].abs() > 0.6;

    let mut t = solve_linear_intercept_time(
        muzzle,
        target_com,
        target_vel,
        bullet_speed,
        shot_origin_offset,
    );

    let mut lead = target_com;
    for _ in 0..2 {
        let damp = if t > 0.35 {
            0.78 + 0.22 * (0.35 / t)
        } else {
            1.0
        };
        let px = target_feet[0] + target_vel[0] * t * damp;
        let py = target_feet[1] + target_vel[1] * t * damp;
        let mut pz_feet = if is_airborne {
            let az = if est_accel_z < -5.0 {
                est_accel_z.clamp(-gravity, 0.0)
            } else {
                -gravity
            };
            target_feet[2] + target_vel[2] * t + 0.5 * az * t * t
        } else {
            target_feet[2]
        };

        let surf = map.surface_height(px, py, target_feet[2].max(pz_feet));
        if pz_feet < surf {
            pz_feet = surf;
        }
        lead = [px, py, pz_feet + com_offset_z];
        let dist = dist3d(muzzle, lead);
        t = ((dist - shot_origin_offset).max(0.0) / bullet_speed.max(1.0)).min(2.0);
    }

    let mut aim = direct_aim(muzzle, lead);
    aim[1] = aim[1].clamp(min_pitch, max_pitch);
    (aim, lead)
}

fn solve_linear_intercept_time(
    muzzle: [f64; 3],
    target_pos: [f64; 3],
    target_vel: [f64; 3],
    bullet_speed: f64,
    shot_origin_offset: f64,
) -> f64 {
    let dx = target_pos[0] - muzzle[0];
    let dy = target_pos[1] - muzzle[1];
    let dz = target_pos[2] - muzzle[2];
    let vx = target_vel[0];
    let vy = target_vel[1];
    let vz = target_vel[2];

    let a = (vx * vx + vy * vy + vz * vz) - bullet_speed * bullet_speed;
    let b = 2.0 * (dx * vx + dy * vy + dz * vz);
    let c = dx * dx + dy * dy + dz * dz;

    let t0 = if a.abs() < 1e-6 {
        if b.abs() > 1e-6 && (-c / b) > 0.0 {
            -c / b
        } else {
            c.sqrt() / bullet_speed.max(1.0)
        }
    } else {
        let disc = b * b - 4.0 * a * c;
        if disc < 0.0 {
            c.sqrt() / bullet_speed.max(1.0)
        } else {
            let sq = disc.sqrt();
            let t1 = (-b - sq) / (2.0 * a);
            let t2 = (-b + sq) / (2.0 * a);
            let mut best = f64::INFINITY;
            if t1 > 0.0 {
                best = best.min(t1);
            }
            if t2 > 0.0 {
                best = best.min(t2);
            }
            if best.is_finite() {
                best
            } else {
                c.sqrt() / bullet_speed.max(1.0)
            }
        }
    };

    let lead0 = [
        target_pos[0] + vx * t0,
        target_pos[1] + vy * t0,
        target_pos[2] + vz * t0,
    ];
    let dist0 = dist3d(muzzle, lead0);
    ((dist0 - shot_origin_offset).max(0.0) / bullet_speed.max(1.0)).min(2.0)
}

#[inline]
fn slew_aligned(
    cur_aim: [f64; 2],
    target_aim: [f64; 2],
    target_radius: f64,
    target_half_h: f64,
    dist: f64,
    slack_mult: f64,
    extra_rad: f64,
) -> bool {
    let d = dist.max(1.0);
    let yaw_tol = (target_radius * slack_mult / d).atan().clamp(0.012, 0.28) + extra_rad;
    let pitch_tol = (target_half_h * slack_mult / d).atan().clamp(0.016, 0.32) + extra_rad;
    angle_diff(cur_aim[0], target_aim[0]).abs() <= yaw_tol
        && (cur_aim[1] - target_aim[1]).abs() <= pitch_tol
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ActorObs, Offer, OfferChoice, PickupObs, ServerMessage};
    use std::process::{Command, Stdio};

    fn load_sample_hello_and_obs() -> (Hello, Observation) {
        let out = Command::new("gzip")
            .args(["-dc", "mechfight-kit/check/stream.jsonl.gz"])
            .stdout(Stdio::piped())
            .output()
            .expect("gzip should decompress stream.jsonl.gz");
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).expect("stream is valid utf8");
        let mut hello: Option<Hello> = None;
        let mut sample_obs: Option<Observation> = None;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<ServerMessage>(line).unwrap() {
                ServerMessage::Hello(mut h) => {
                    h.map.dir = "mechfight-kit/arena".to_string();
                    hello = Some(*h);
                }
                ServerMessage::Obs(obs) if sample_obs.is_none() => {
                    sample_obs = Some(*obs);
                    break;
                }
                _ => {}
            }
        }
        (hello.unwrap(), sample_obs.unwrap())
    }

    #[test]
    fn laser_charge_and_release_state_machine() {
        let (hello, obs) = load_sample_hello_and_obs();
        let bot = Bot::new(hello);
        let mut w_laser = WeaponObs {
            kind: WeaponKind::Laser,
            cooldown: 0.0,
            ammo: None,
            reload: None,
            charge: Some(0.20),
            stats: bot.config.weapons.laser.clone(),
        };
        let tgt_healthy = TargetCandidate {
            id: 5,
            kind: TargetKind::Mech,
            pos: [100.0, 0.0, 60.0],
            vel: [0.0, 0.0, 0.0],
            est_accel_z: 0.0,
            aim: [0.0, 0.0],
            com_z: 64.5,
            radius: 2.5,
            hp: 1000.0,
            max_hp: 1400.0,
            weapons: vec![WeaponKind::Gun],
            charging: false,
            has_los: true,
            dist: 100.0,
        };
        let muzzle = [0.0, 0.0, 67.0];
        assert!(bot.map.has_los(muzzle, [100.0, 0.0, 64.5]));

        // 1. Mid-charge against healthy enemy -> keep holding
        let (_, hold1) = bot.eval_laser([0.0, 0.0], muzzle, 0, &w_laser, &tgt_healthy, -0.7, 1.3);
        assert!(hold1, "should keep holding while charging at 0.20s");

        // 2. Full charge + aligned -> release (false)
        w_laser.charge = Some(0.72);
        let (_, hold2) = bot.eval_laser([0.0, -0.025], muzzle, 0, &w_laser, &tgt_healthy, -0.7, 1.3);
        assert!(!hold2, "should release laser at >= 90% full charge when aligned");

        // 3. Partial charge lethal with cookoff (+60%) + rear armor (x1.5) -> release early!
        w_laser.charge = Some(0.35);
        let mut tgt_mortar_rear = tgt_healthy.clone();
        tgt_mortar_rear.hp = 350.0;
        tgt_mortar_rear.weapons = vec![WeaponKind::Mortar];
        tgt_mortar_rear.aim = [0.0, 0.0]; // facing +x, muzzle at 0 is behind it
        let (_, hold3) =
            bot.eval_laser([0.0, -0.025], muzzle, 0, &w_laser, &tgt_mortar_rear, -0.7, 1.3);
        assert!(!hold3, "should release partial charge for lethal mortar cookoff/rear finish");

        // 4. Approaching hold_s forced timeout -> release cleanly
        w_laser.charge = Some(2.60);
        let (_, hold4) = bot.eval_laser([1.5, 0.0], muzzle, 0, &w_laser, &tgt_healthy, -0.7, 1.3);
        assert!(!hold4, "must release before hold_s timeout");
        let _ = obs;
    }

    #[test]
    fn laser_slew_gate_blocks_off_target_release() {
        let (hello, _) = load_sample_hello_and_obs();
        let bot = Bot::new(hello);
        let w_laser = WeaponObs {
            kind: WeaponKind::Laser,
            cooldown: 0.0,
            ammo: None,
            reload: None,
            charge: Some(0.74),
            stats: bot.config.weapons.laser.clone(),
        };
        let tgt = TargetCandidate {
            id: 5,
            kind: TargetKind::Mech,
            pos: [100.0, 0.0, 60.0],
            vel: [0.0, 0.0, 0.0],
            est_accel_z: 0.0,
            aim: [0.0, 0.0],
            com_z: 64.5,
            radius: 2.5,
            hp: 1000.0,
            max_hp: 1400.0,
            weapons: vec![WeaponKind::Gun],
            charging: false,
            has_los: true,
            dist: 100.0,
        };
        let muzzle = [0.0, 0.0, 67.0];
        // 0.15 rad (~8.6 deg) off target at 100 m must NOT release!
        let (_, hold) = bot.eval_laser([0.15, -0.025], muzzle, 0, &w_laser, &tgt, -0.7, 1.3);
        assert!(hold, "must keep holding while 0.15 rad off target at 100 m");
    }

    #[test]
    fn finds_ground_pickup_on_sloped_terrain() {
        let (hello, mut obs) = load_sample_hello_and_obs();
        let mut bot = Bot::new(hello);
        obs.t = 5.0;
        // Simulate stream.jsonl.gz scenario: bot at low ground (-1.93 m), Laser on slope at +3.2 m
        let mz = bot.map.ground_height(310.0, -275.0);
        let pz = bot.map.ground_height(305.1, -129.7);
        obs.self_obs.pos = [310.0, -275.0, mz];
        obs.self_obs.hp = 1400.0;
        obs.self_obs.max_hp = 1400.0;
        obs.self_obs.in_zone = true;
        obs.zone.center = [41.3, -72.8];
        obs.zone.radius = 560.0;
        obs.zone.next_center = [-42.1, 7.1];
        obs.zone.next_radius = 380.0;
        obs.zone.shrink_start = 60.0;
        obs.pickups = vec![PickupObs {
            id: 8022,
            kind: WeaponKind::Laser,
            pos: [305.1, -129.7, pz],
        }];
        bot.act(&obs);
        assert_eq!(
            bot.posture,
            Posture::LootWeapon,
            "must seek ground-level Laser pickup across sloped campus elevation"
        );
    }

    #[test]
    fn dynamic_upgrade_scorer_uses_config_and_held_weapons() {
        let (hello, obs) = load_sample_hello_and_obs();
        let bot = Bot::new(hello);
        let mut me = obs.self_obs.clone();
        // Currently holding only Gun
        let offer1 = Offer {
            id: 1,
            expires: 30.0,
            choices: vec![
                OfferChoice {
                    id: "mortar_splash".into(),
                    name: "Cluster shells".into(),
                    desc: "".into(),
                },
                OfferChoice {
                    id: "armor".into(),
                    name: "Reactive armor".into(),
                    desc: "".into(),
                },
                OfferChoice {
                    id: "capacitor".into(),
                    name: "Capacitor bank".into(),
                    desc: "".into(),
                },
            ],
        };
        assert_eq!(bot.pick_upgrade(&offer1, &me), 1, "armor should win Tier S");

        // Unheld weapon upgrade vs held gun_rate
        let offer2 = Offer {
            id: 2,
            expires: 30.0,
            choices: vec![
                OfferChoice {
                    id: "laser_damage".into(),
                    name: "Focusing lens".into(),
                    desc: "".into(),
                },
                OfferChoice {
                    id: "gun_rate".into(),
                    name: "Rotary feed".into(),
                    desc: "".into(),
                },
                OfferChoice {
                    id: "capacitor".into(),
                    name: "Capacitor bank".into(),
                    desc: "".into(),
                },
            ],
        };
        assert_eq!(
            bot.pick_upgrade(&offer2, &me),
            1,
            "held gun_rate should beat unheld laser_damage"
        );

        // Once sensors taken once, nanites should win
        me.upgrades.push("sensors".into());
        let offer3 = Offer {
            id: 3,
            expires: 30.0,
            choices: vec![
                OfferChoice {
                    id: "sensors".into(),
                    name: "Sensor suite".into(),
                    desc: "".into(),
                },
                OfferChoice {
                    id: "nanites".into(),
                    name: "Repair nanites".into(),
                    desc: "".into(),
                },
                OfferChoice {
                    id: "gyros".into(),
                    name: "Torso gyros".into(),
                    desc: "".into(),
                },
            ],
        };
        assert_eq!(bot.pick_upgrade(&offer3, &me), 1, "nanites should win");
    }

    #[test]
    fn take_failed_blacklists_vanished_pickup() {
        let (hello, mut obs) = load_sample_hello_and_obs();
        let mut bot = Bot::new(hello);
        obs.t = 15.0;
        let gz = bot.map.ground_height(20.0, 20.0);
        obs.pickups = vec![PickupObs {
            id: 8042,
            kind: WeaponKind::Laser,
            pos: [20.0, 20.0, gz],
        }];
        bot.act(&obs);
        assert!(bot.last_pickup_seen.contains_key(&8042));

        obs.t = 15.2;
        obs.events = vec![GameEvent::TakeFailed {
            id: 8042,
            reason: "gone".into(),
        }];
        bot.act(&obs);
        assert!(
            !bot.last_pickup_seen.contains_key(&8042),
            "TakeFailed(gone) must remove and blacklist pickup"
        );
    }

    #[test]
    fn airborne_lead_applies_gravity_and_clamps_floor() {
        let map = Map::fallback_empty();
        let muzzle = [0.0, 0.0, 7.0];
        let (_, lead_clamp) = solve_gun_lead_airborne(
            &map,
            muzzle,
            [100.0, 0.0, 1.0],
            [0.0, 0.0, -10.0],
            -22.0,
            4.5,
            22.0,
            252.0,
            3.0,
            -0.7,
            1.3,
        );
        assert!(
            lead_clamp[2] >= 4.49,
            "falling target lead z must clamp to floor + COM: {lead_clamp:?}"
        );

        let (_, lead_arc) = solve_gun_lead_airborne(
            &map,
            muzzle,
            [120.0, 0.0, 30.0],
            [0.0, 0.0, 8.0],
            -22.0,
            4.5,
            22.0,
            252.0,
            3.0,
            -0.7,
            1.3,
        );
        let t_approx = (120.0 - 3.0) / 252.0;
        let naive_linear_z = 34.5 + 8.0 * t_approx;
        assert!(
            lead_arc[2] < naive_linear_z - 1.0,
            "airborne gravity lead ({}) should sit below linear ({naive_linear_z})",
            lead_arc[2]
        );
    }

    #[test]
    fn emergency_regen_holds_fire_and_shields_rear() {
        let (hello, mut obs) = load_sample_hello_and_obs();
        let mut bot = Bot::new(hello);
        // Position near building block 14 so cover exists within 48 m
        obs.t = 40.0;
        let mz = bot.map.ground_height(86.0, -110.0);
        obs.self_obs.pos = [86.0, -110.0, mz];
        obs.self_obs.hp = 500.0; // < 42% of 1400
        obs.self_obs.max_hp = 1400.0;
        obs.self_obs.aim = [0.0, 0.0];
        obs.self_obs.in_zone = true;
        obs.zone.center = [0.0, 0.0];
        obs.zone.radius = 400.0;
        obs.zone.next_center = [0.0, 0.0];
        obs.zone.next_radius = 350.0;
        obs.zone.shrink_start = 90.0;
        obs.events = vec![GameEvent::Damaged {
            amount: 45.0,
            weapon: "gun".into(),
            bearing: Some(2.4),
            by: Some(5),
            rear: true,
        }];
        let ez = bot.map.ground_height(75.0, -140.0);
        obs.self_obs.aim = direct_aim([86.0, -110.0, mz + 7.0], [75.0, -140.0, ez + 4.5]);
        obs.mechs = vec![ActorObs {
            id: 5,
            kind: "mech".into(),
            pos: [75.0, -140.0, ez],
            vel: [0.0, 0.0, 0.0],
            aim: [1.3, 0.0],
            hp: Some(1400.0),
            max_hp: Some(1400.0),
            cubes: Some(0),
            weapons: vec![WeaponKind::Gun],
            charging: false,
        }];
        let act1 = bot.act(&obs);
        assert_eq!(bot.posture, Posture::EmergencyRegen);
        assert!(act1.fire.unwrap()[0], "should fire back while taking fire in LOS on way to cover");

        // Once behind cover (no LOS enemy) -> strictly holds fire & shields toward damage bearing
        obs.t = 40.3;
        obs.events.clear();
        obs.mechs.clear();
        let act2 = bot.act(&obs);
        assert_eq!(bot.posture, Posture::EmergencyRegen);
        assert_eq!(act2.fire.unwrap(), [false, false]);
        assert!(
            angle_diff(act2.aim.unwrap()[0], 2.4).abs() < 1e-6,
            "EmergencyRegen should shield rear toward damage bearing when hidden"
        );
    }

    #[test]
    fn dual_fire_gun_and_lightning_simultaneously() {
        let (hello, mut obs) = load_sample_hello_and_obs();
        let mut bot = Bot::new(hello);
        obs.t = 45.0;
        let mz = bot.map.ground_height(0.0, 0.0);
        obs.self_obs.pos = [0.0, 0.0, mz];
        obs.self_obs.hp = 1400.0;
        obs.self_obs.max_hp = 1400.0;
        obs.self_obs.energy = 100.0;
        obs.self_obs.aim = [0.0, 0.0];
        obs.self_obs.in_zone = true;
        obs.zone.center = [0.0, 0.0];
        obs.zone.radius = 400.0;
        obs.zone.next_center = [0.0, 0.0];
        obs.zone.next_radius = 350.0;
        obs.zone.shrink_start = 90.0;

        let gun = obs.self_obs.weapons[0].clone().unwrap();
        let lt = WeaponObs {
            kind: WeaponKind::Lightning,
            cooldown: 0.0,
            ammo: None,
            reload: None,
            charge: None,
            stats: bot.config.weapons.lightning.clone(),
        };
        obs.self_obs.weapons = vec![Some(gun), Some(lt)];

        let ez = bot.map.ground_height(30.0, 0.0);
        obs.mechs = vec![ActorObs {
            id: 4,
            kind: "mech".into(),
            pos: [30.0, 0.0, ez],
            vel: [0.0, 0.0, 0.0],
            aim: [PI, 0.0],
            hp: Some(1200.0),
            max_hp: Some(1400.0),
            cubes: Some(0),
            weapons: vec![WeaponKind::Gun],
            charging: false,
        }];

        let act = bot.act(&obs);
        assert_eq!(act.fire.unwrap(), [true, true]);
    }

    #[test]
    fn slew_gate_rejects_large_misalignment() {
        assert!(!slew_aligned(
            [0.0, 0.0],
            [0.15, 0.0],
            2.5,
            4.5,
            100.0,
            0.95,
            0.0
        ));
        assert!(slew_aligned(
            [0.0, 0.0],
            [0.02, 0.02],
            2.5,
            4.5,
            100.0,
            1.25,
            0.012
        ));
    }

    #[test]
    fn action_sanitize_strips_nan_and_normalizes_move() {
        let mut a = Action {
            tick: 10,
            move_dir: Some([3.0, 4.0]),
            aim: Some([f64::NAN, 5.0]),
            ..Default::default()
        };
        a.sanitize(-0.7, 1.3, [0.5, 0.0]);
        let mv = a.move_dir.unwrap();
        assert!((mv[0].hypot(mv[1]) - 1.0).abs() < 1e-9);
        let aim = a.aim.unwrap();
        assert_eq!(aim[0], 0.5);
        assert_eq!(aim[1], 1.3);
        let json = serde_json::to_string(&a).unwrap();
        assert!(!json.contains("null"));
        assert!(!json.contains("NaN"));
    }

    #[test]
    fn deserializes_entire_stream_and_faces_damage_bearing() {
        let out = Command::new("gzip")
            .args(["-dc", "mechfight-kit/check/stream.jsonl.gz"])
            .stdout(Stdio::piped())
            .output()
            .expect("gzip should decompress stream.jsonl.gz");
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).expect("stream is valid utf8");

        let mut bot: Option<Bot> = None;
        let mut obs_count = 0usize;
        let mut sample_obs: Option<Observation> = None;

        for (line_no, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let msg: ServerMessage = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("line {} failed to parse: {e}", line_no + 1));
            match msg {
                ServerMessage::Hello(mut h) => {
                    h.map.dir = "mechfight-kit/arena".to_string();
                    bot = Some(Bot::new(*h));
                }
                ServerMessage::Obs(obs) => {
                    obs_count += 1;
                    if sample_obs.is_none() && obs.mechs.is_empty() {
                        sample_obs = Some((*obs).clone());
                    }
                    let b = bot.as_mut().expect("hello must precede obs");
                    let mut act = b.act(&obs);
                    act.sanitize(-0.7, 1.3, obs.self_obs.aim);
                    let ser = serde_json::to_string(&act).unwrap();
                    assert!(!ser.contains("null"));
                }
                ServerMessage::End(_) => {}
                ServerMessage::Unknown => panic!("unexpected unknown message at line {}", line_no + 1),
            }
        }
        assert_eq!(obs_count, 300);

        let mut dmg_obs = sample_obs.expect("sample obs without visible mechs");
        dmg_obs.t = 50.0;
        dmg_obs.crates.clear();
        dmg_obs.events = vec![GameEvent::Damaged {
            amount: 45.0,
            weapon: "gun".into(),
            bearing: Some(-1.25),
            by: Some(7),
            rear: true,
        }];
        let b = bot.as_mut().unwrap();
        let act = b.act(&dmg_obs);
        assert!(
            angle_diff(act.aim.unwrap()[0], -1.25).abs() < 1e-6,
            "bot should orient torso toward attacker damage bearing"
        );
    }
}

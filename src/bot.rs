use std::f64::consts::PI;

use crate::map::Map;
use crate::protocol::{
    stat, Action, Config, DebugDraw, DebugLine, DebugPoint, GameEvent, Hello, Observation,
    WeaponKind, WeaponObs,
};

#[derive(Clone, Copy)]
enum TargetKind {
    Mech,
    Npc,
    Crate,
}

#[derive(Clone, Copy)]
struct TargetCandidate {
    id: u64,
    kind: TargetKind,
    pos: [f64; 3],
    vel: [f64; 3],
    com_z: f64,
    radius: f64,
    hp: f64,
    charging: bool,
    has_los: bool,
    dist: f64,
}

pub struct Bot {
    name: String,
    config: Config,
    map: Map,
    last_damage_bearing: Option<f64>,
    last_damage_t: f64,
    last_damage_by: Option<u64>,
    last_sound_bearing: Option<f64>,
    last_sound_t: f64,
    last_sound_priority: f64,
    last_move: [f64; 2],
    strafe_sign: f64,
    last_strafe_flip_t: f64,
    stuck_since_t: Option<f64>,
    last_combat_t: f64,
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
        let strafe_sign = if hello.seed % 2 == 0 { 1.0 } else { -1.0 };
        Self {
            name: "rust-bot-m1".to_string(),
            config: hello.config,
            map,
            last_damage_bearing: None,
            last_damage_t: -100.0,
            last_damage_by: None,
            last_sound_bearing: None,
            last_sound_t: -100.0,
            last_sound_priority: 0.0,
            last_move: [0.0, 0.0],
            strafe_sign,
            last_strafe_flip_t: -100.0,
            stuck_since_t: None,
            last_combat_t: -100.0,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn act(&mut self, obs: &Observation) -> Action {
        let me = &obs.self_obs;
        let t = obs.t;

        self.update_memory(obs);

        let mut action = Action {
            tick: obs.tick,
            ..Default::default()
        };

        if let Some(offer) = &obs.offer {
            action.pick = Some(self.pick_upgrade(offer));
        }

        if let Some(pickup_id) = self.pick_weapon_take(obs) {
            action.take = Some(pickup_id);
        }

        let muzzle_h = stat(&me.stats, "muzzle_height", 7.0);
        let muzzle = [me.pos[0], me.pos[1], me.pos[2] + muzzle_h];
        let mech_radius = stat(&me.stats, "radius", 2.5);
        let mech_height = stat(&me.stats, "height", 9.0);
        let min_pitch = stat(&me.stats, "min_pitch", -0.7);
        let max_pitch = stat(&me.stats, "max_pitch", 1.3);
        let turn_rate = stat(&me.stats, "turn_rate", 4.0);
        let pitch_rate = stat(&me.stats, "pitch_rate", 3.0);

        let (primary_mech, primary_farm, any_visible_mech) =
            self.select_targets(obs, muzzle);

        let target = primary_mech.or(primary_farm);

        // Combat aim + weapon firing
        let mut fire = [false, false];
        let mut reload = false;
        let mut chosen_aim: Option<[f64; 2]> = None;
        let mut debug_aim_target: Option<[f64; 3]> = None;

        // If we are actively charging a laser in any slot, check if we must maintain/release it
        let mut active_laser_slot: Option<usize> = None;
        for (idx, wopt) in me.weapons.iter().enumerate().take(2) {
            if let Some(w) = wopt {
                if w.kind == WeaponKind::Laser && w.charge.unwrap_or(0.0) > 0.0 {
                    active_laser_slot = Some(idx);
                }
            }
        }

        if let Some(tgt) = target {
            debug_aim_target = Some([tgt.pos[0], tgt.pos[1], tgt.com_z]);

            if let Some(l_slot) = active_laser_slot {
                if let Some(w) = me.weapons[l_slot].as_ref() {
                    let (aim, should_hold) = self.eval_laser(
                        me.aim,
                        me.pos,
                        muzzle,
                        w,
                        &tgt,
                        turn_rate,
                        pitch_rate,
                        min_pitch,
                        max_pitch,
                    );
                    chosen_aim = Some(aim);
                    fire[l_slot] = should_hold;
                }
            } else {
                // Choose best weapon slot to own torso aim this tick
                let best_slot = self.choose_weapon_slot(obs, muzzle, mech_radius, mech_height, &tgt);
                if let Some(slot) = best_slot {
                    if let Some(w) = me.weapons[slot].as_ref() {
                        match w.kind {
                            WeaponKind::Lightning => {
                                let (aim, should_fire) =
                                    self.eval_lightning(me.aim, me.energy, me.max_energy, muzzle, w, &tgt);
                                chosen_aim = Some(aim);
                                fire[slot] = should_fire;
                            }
                            WeaponKind::Laser => {
                                let (aim, should_hold) = self.eval_laser(
                                    me.aim,
                                    me.pos,
                                    muzzle,
                                    w,
                                    &tgt,
                                    turn_rate,
                                    pitch_rate,
                                    min_pitch,
                                    max_pitch,
                                );
                                chosen_aim = Some(aim);
                                fire[slot] = should_hold;
                            }
                            WeaponKind::Gun => {
                                let (aim, should_fire, should_reload) = self.eval_gun(
                                    me.aim,
                                    muzzle,
                                    mech_radius,
                                    w,
                                    &tgt,
                                    turn_rate,
                                    pitch_rate,
                                    min_pitch,
                                    max_pitch,
                                );
                                chosen_aim = Some(aim);
                                fire[slot] = should_fire;
                                reload = should_reload;
                            }
                            WeaponKind::Mortar => {
                                if let Some((aim, should_fire)) = self.eval_mortar(
                                    me.aim,
                                    me.pos,
                                    mech_height,
                                    w,
                                    &tgt,
                                    turn_rate,
                                    pitch_rate,
                                    min_pitch,
                                    max_pitch,
                                ) {
                                    chosen_aim = Some(aim);
                                    fire[slot] = should_fire;
                                } else {
                                    chosen_aim = Some(direct_aim(muzzle, [tgt.pos[0], tgt.pos[1], tgt.com_z]));
                                }
                            }
                        }
                    }
                } else {
                    chosen_aim = Some(direct_aim(muzzle, [tgt.pos[0], tgt.pos[1], tgt.com_z]));
                }
            }
        } else if let Some(l_slot) = active_laser_slot {
            // Lost target while laser is charging: keep holding slightly if below min_charge or release
            if let Some(w) = me.weapons[l_slot].as_ref() {
                let charge = w.charge.unwrap_or(0.0);
                let hold_s = stat(&w.stats, "hold_s", 2.0);
                let full_charge = stat(&w.stats, "charge", 0.75);
                fire[l_slot] = charge < full_charge + hold_s * 0.5;
            }
        }

        // Reload gun opportunistically when not actively firing at a visible LOS target
        if !reload && target.is_none_or(|tg| !tg.has_los) {
            for wopt in me.weapons.iter().flatten() {
                if wopt.kind == WeaponKind::Gun && wopt.reload.unwrap_or(0.0) <= 0.0 {
                    let mag = stat(&wopt.stats, "mag", 12.0);
                    let ammo = wopt.ammo.unwrap_or(mag);
                    if ammo <= mag * 0.5 {
                        reload = true;
                        break;
                    }
                }
            }
        }

        // Movement & Context Steering
        let (goal_dir, goal_pt, urgent_zone, dodged_hazard) =
            self.compute_goal_direction(obs, primary_mech.or(any_visible_mech));

        let (move_dir, blocked_ahead) = self.steer(obs, goal_dir, mech_radius, mech_height);
        self.last_move = move_dir;

        // Rear-armor shielding when no active combat target owns aim
        let final_aim = if let Some(aim) = chosen_aim {
            aim
        } else if t - self.last_damage_t <= 3.0 {
            if let Some(b) = self.last_damage_bearing {
                [b, 0.0]
            } else {
                [me.aim[0], 0.0]
            }
        } else if let Some(m) = any_visible_mech {
            let yaw = (m.pos[1] - me.pos[1]).atan2(m.pos[0] - me.pos[0]);
            [yaw, 0.0]
        } else if t - self.last_sound_t <= 2.5 {
            if let Some(b) = self.last_sound_bearing {
                [b, 0.0]
            } else {
                [me.aim[0], 0.0]
            }
        } else {
            let move_yaw = if move_dir[0].hypot(move_dir[1]) > 0.1 {
                move_dir[1].atan2(move_dir[0])
            } else {
                me.aim[0]
            };
            [wrap_angle(move_yaw + 0.45 * (t * 1.4).sin()), 0.0]
        };

        // Boost & jump decisions
        let dist_next = dist2d(me.pos[0], me.pos[1], obs.zone.next_center[0], obs.zone.next_center[1]);
        let safe_r = (obs.zone.next_radius - 18.0).max(0.0);
        let low_hp_retreat = me.hp < 0.35 * me.max_hp && any_visible_mech.is_some();
        let moving = move_dir[0].hypot(move_dir[1]) > 0.25;

        let boost = me.grounded
            && moving
            && ((!me.in_zone && me.energy > 5.0)
                || (dodged_hazard && me.energy > 15.0)
                || (urgent_zone && dist_next > safe_r && me.energy > 28.0)
                || (low_hp_retreat && me.energy > 25.0));

        let stuck_jump = me.grounded
            && self
                .stuck_since_t
                .is_some_and(|st| t - st > 0.5 || blocked_ahead);

        if stuck_jump && t - self.last_strafe_flip_t > 0.8 {
            self.strafe_sign = -self.strafe_sign;
            self.last_strafe_flip_t = t;
        }

        action.move_dir = Some(move_dir);
        action.aim = Some(final_aim);
        action.boost = Some(boost);
        action.jump = Some(stuck_jump);
        action.fire = Some(fire);
        if reload {
            action.reload = Some(true);
        }

        // Minimal debug draw strictly under caps
        let max_chars = self.config.debug.max_text_chars.min(72);
        let tgt_str = match target {
            Some(tg) => format!("t={}", tg.id),
            None => "patrol".to_string(),
        };
        let mut label = format!(
            "hp={:.0} en={:.0} c={} {}",
            me.hp, me.energy, me.cubes, tgt_str
        );
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

    fn update_memory(&mut self, obs: &Observation) {
        let t = obs.t;
        let me = &obs.self_obs;

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
                _ => {}
            }
        }

        // Decay sound memory priority
        if t - self.last_sound_t > 2.5 {
            self.last_sound_priority = 0.0;
        }
        for s in &obs.sounds {
            let s_time = t - s.age;
            if s_time < self.last_sound_t - 0.5 && self.last_sound_priority > 0.0 {
                continue;
            }
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
            }
        }

        // Stuck detection
        let cmd_speed = self.last_move[0].hypot(self.last_move[1]);
        let actual_speed = me.vel[0].hypot(me.vel[1]);
        if cmd_speed > 0.5 && actual_speed < 1.5 && me.grounded {
            if self.stuck_since_t.is_none() {
                self.stuck_since_t = Some(t);
            }
        } else {
            self.stuck_since_t = None;
        }

        // Periodic strafe flip
        if t - self.last_strafe_flip_t > 2.6 {
            self.strafe_sign = -self.strafe_sign;
            self.last_strafe_flip_t = t;
        }
    }

    fn pick_upgrade(&self, offer: &crate::protocol::Offer) -> usize {
        let mut best_idx = 0usize;
        let mut best_score = f64::NEG_INFINITY;

        for (i, choice) in offer.choices.iter().enumerate() {
            let score = if let Some(def) = self.config.upgrades.iter().find(|u| u.id == choice.id) {
                let mut max_eff = 10.0_f64;
                for eff in &def.effects {
                    let s = match eff.path.as_str() {
                        "mech.armor" => 90.0,
                        "mech.hp" => 85.0,
                        "weapons.gun.cooldown" => 82.0,
                        "weapons.gun.damage" => 80.0,
                        "mech.regen_per_s" | "mech.regen_delay" => 78.0,
                        "weapons.laser.damage" | "weapons.laser.charge" | "weapons.laser.pierce" => 75.0,
                        "weapons.lightning.damage"
                        | "weapons.lightning.energy"
                        | "weapons.lightning.range"
                        | "weapons.lightning.chain"
                        | "weapons.lightning.cone_deg" => 72.0,
                        "mech.walk_speed" | "mech.boost_speed" => 65.0,
                        "weapons.gun.speed" | "weapons.gun.range" => 62.0,
                        "weapons.mortar.cooldown"
                        | "weapons.mortar.splash_radius"
                        | "weapons.mortar.speed" => 60.0,
                        "weapons.gun.mag" | "weapons.gun.reload" => 55.0,
                        "mech.energy_regen" | "mech.boost_drain" | "mech.jet_drain" => 52.0,
                        "mech.turn_rate" | "mech.pitch_rate" => 50.0,
                        "mech.near_range_bonus" | "mech.vision_bonus" | "mech.hearing" => 45.0,
                        "mech.energy" => 40.0,
                        _ => 25.0,
                    };
                    max_eff = max_eff.max(s);
                }
                max_eff + 4.0 * (def.effects.len().saturating_sub(1) as f64)
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

    fn pick_weapon_take(&self, obs: &Observation) -> Option<u64> {
        let me = &obs.self_obs;
        if me.weapons.get(1).and_then(|w| w.as_ref()).is_some() {
            return None;
        }
        let reach = (self.config.loot.pickup_radius - 0.1).max(0.5);
        let mut best: Option<(u8, f64, u64)> = None;

        for p in &obs.pickups {
            if p.kind == WeaponKind::Gun {
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
            let prio = match p.kind {
                WeaponKind::Laser => 3,
                WeaponKind::Lightning => 2,
                WeaponKind::Mortar => 1,
                WeaponKind::Gun => 0,
            };
            let better = match best {
                None => true,
                Some((bp, bd, _)) => prio > bp || (prio == bp && d2d < bd),
            };
            if better {
                best = Some((prio, d2d, p.id));
            }
        }

        best.map(|(_, _, id)| id)
    }

    fn select_targets(
        &self,
        obs: &Observation,
        muzzle: [f64; 3],
    ) -> (
        Option<TargetCandidate>,
        Option<TargetCandidate>,
        Option<TargetCandidate>,
    ) {
        let me = &obs.self_obs;
        let base_mech_h = stat(&self.config.mech, "height", 9.0);
        let base_mech_r = stat(&self.config.mech, "radius", 2.5);
        let size_per_cube = self.config.cubes.size_per_cube;
        let npc_r = stat(&self.config.npc, "radius", 1.8);
        let npc_h = stat(&self.config.npc, "height", 3.5);
        let crate_r = stat(&self.config.crates, "radius", 1.4);
        let crate_h = stat(&self.config.crates, "height", 2.4);

        let mut best_los_mech: Option<(f64, TargetCandidate)> = None;
        let mut best_mem_mech: Option<(f64, TargetCandidate)> = None;
        let mut best_farm: Option<(f64, TargetCandidate)> = None;

        for actor in &obs.mechs {
            let d = dist3d(muzzle, actor.pos);
            if actor.kind == "mech" {
                let scale = 1.0 + size_per_cube * (actor.cubes.unwrap_or(0) as f64);
                let com_z = actor.pos[2] + 0.5 * base_mech_h * scale;
                let radius = base_mech_r * scale;
                let has_los = self.map.has_los(muzzle, [actor.pos[0], actor.pos[1], com_z]);
                let hp = actor.hp.unwrap_or(1400.0);
                let cand = TargetCandidate {
                    id: actor.id,
                    kind: TargetKind::Mech,
                    pos: actor.pos,
                    vel: actor.vel,
                    com_z,
                    radius,
                    hp,
                    charging: actor.charging,
                    has_los,
                    dist: d,
                };

                let mut score = -d;
                if actor.hp.is_some() {
                    score += (1400.0 - hp).max(0.0) * 0.05;
                }
                if self.last_damage_by == Some(actor.id) && obs.t - self.last_damage_t <= 3.0 {
                    score += 35.0;
                }
                if actor.charging {
                    score += 25.0;
                }

                if has_los {
                    if best_los_mech.is_none_or(|(bs, _)| score > bs) {
                        best_los_mech = Some((score, cand));
                    }
                } else if best_mem_mech.is_none_or(|(bs, _)| score > bs) {
                    best_mem_mech = Some((score, cand));
                }
            } else if actor.kind == "npc" && d <= 95.0 && me.hp > 0.45 * me.max_hp && obs.t - self.last_damage_t > 3.0 {
                let com_z = actor.pos[2] + 0.5 * npc_h;
                let has_los = self.map.has_los(muzzle, [actor.pos[0], actor.pos[1], com_z]);
                if has_los {
                    let cand = TargetCandidate {
                        id: actor.id,
                        kind: TargetKind::Npc,
                        pos: actor.pos,
                        vel: actor.vel,
                        com_z,
                        radius: npc_r,
                        hp: actor.hp.unwrap_or(350.0),
                        charging: false,
                        has_los: true,
                        dist: d,
                    };
                    let score = 200.0 - d;
                    if best_farm.is_none_or(|(bs, _)| score > bs) {
                        best_farm = Some((score, cand));
                    }
                }
            }
        }

        if best_los_mech.is_none()
            && best_farm.is_none()
            && me.hp > 0.45 * me.max_hp
            && obs.t - self.last_damage_t > 3.0
        {
            for c in &obs.crates {
                let d = dist3d(muzzle, c.pos);
                if d > 55.0 {
                    continue;
                }
                let com_z = c.pos[2] + 0.5 * crate_h;
                if self.map.has_los(muzzle, [c.pos[0], c.pos[1], com_z]) {
                    let cand = TargetCandidate {
                        id: c.id,
                        kind: TargetKind::Crate,
                        pos: c.pos,
                        vel: [0.0, 0.0, 0.0],
                        com_z,
                        radius: crate_r,
                        hp: c.hp.unwrap_or(300.0),
                        charging: false,
                        has_los: true,
                        dist: d,
                    };
                    let score = 100.0 - d;
                    if best_farm.is_none_or(|(bs, _)| score > bs) {
                        best_farm = Some((score, cand));
                    }
                }
            }
        }

        let any_mech = best_los_mech.or(best_mem_mech).map(|(_, c)| c);
        let primary_mech = best_los_mech.map(|(_, c)| c).or_else(|| {
            // Only pick a non-LOS mech as combat target if we carry a mortar to lob over cover
            if me.weapons.iter().flatten().any(|w| w.kind == WeaponKind::Mortar) {
                best_mem_mech.map(|(_, c)| c)
            } else {
                None
            }
        });

        (primary_mech, best_farm.map(|(_, c)| c), any_mech)
    }

    fn choose_weapon_slot(
        &self,
        obs: &Observation,
        muzzle: [f64; 3],
        _mech_radius: f64,
        mech_height: f64,
        tgt: &TargetCandidate,
    ) -> Option<usize> {
        let me = &obs.self_obs;
        let min_pitch = stat(&me.stats, "min_pitch", -0.7);
        let max_pitch = stat(&me.stats, "max_pitch", 1.3);
        let mut best: Option<(i32, usize)> = None;

        for (slot, wopt) in me.weapons.iter().enumerate().take(2) {
            let Some(w) = wopt else { continue };
            let prio = match w.kind {
                WeaponKind::Lightning => {
                    let range = stat(&w.stats, "range", 45.0);
                    let e_cost = stat(&w.stats, "energy", 3.0);
                    let has_energy = me.energy >= 0.25 * me.max_energy + e_cost
                        || tgt.charging
                        || tgt.hp < 300.0;
                    if tgt.has_los && tgt.dist <= range - 0.5 && has_energy && w.cooldown <= 0.12 {
                        100
                    } else {
                        -1
                    }
                }
                WeaponKind::Laser => {
                    let range = stat(&w.stats, "range", 280.0);
                    if tgt.has_los
                        && matches!(tgt.kind, TargetKind::Mech)
                        && tgt.dist <= range
                        && (w.cooldown <= 0.08 || w.charge.unwrap_or(0.0) > 0.0)
                    {
                        90
                    } else {
                        -1
                    }
                }
                WeaponKind::Gun => {
                    let range = stat(&w.stats, "range", 230.0);
                    let ammo = w.ammo.unwrap_or(1.0);
                    let reload = w.reload.unwrap_or(0.0);
                    if tgt.has_los && tgt.dist <= range && ammo > 0.0 && reload <= 0.0 {
                        80
                    } else if tgt.has_los && tgt.dist <= range {
                        10
                    } else {
                        -1
                    }
                }
                WeaponKind::Mortar => {
                    let splash_r = stat(&w.stats, "splash_radius", 7.0);
                    let dist2 = dist2d(me.pos[0], me.pos[1], tgt.pos[0], tgt.pos[1]);
                    if w.cooldown <= 0.12
                        && dist2 > splash_r + 10.0
                        && matches!(tgt.kind, TargetKind::Mech)
                        && self
                            .solve_mortar_aim(me.pos, mech_height, w, tgt, min_pitch, max_pitch)
                            .is_some()
                    {
                        if tgt.has_los { 60 } else { 85 }
                    } else {
                        -1
                    }
                }
            };
            if prio >= 0 && best.is_none_or(|(bp, _)| prio > bp) {
                best = Some((prio, slot));
            }
        }

        // unused muzzle silence
        let _ = muzzle;
        best.map(|(_, s)| s)
    }

    #[allow(clippy::too_many_arguments)]
    fn eval_gun(
        &self,
        cur_aim: [f64; 2],
        muzzle: [f64; 3],
        mech_radius: f64,
        w: &WeaponObs,
        tgt: &TargetCandidate,
        turn_rate: f64,
        pitch_rate: f64,
        min_pitch: f64,
        max_pitch: f64,
    ) -> ([f64; 2], bool, bool) {
        let speed = stat(&w.stats, "speed", 252.0);
        let range = stat(&w.stats, "range", 230.0);
        let mag = stat(&w.stats, "mag", 12.0);
        let ammo = w.ammo.unwrap_or(mag);
        let reload_left = w.reload.unwrap_or(0.0);

        let (aim, lead_pos) = solve_gun_lead(
            muzzle,
            [tgt.pos[0], tgt.pos[1], tgt.com_z],
            tgt.vel,
            speed,
            mech_radius + 0.5,
            min_pitch,
            max_pitch,
        );

        let need_reload = reload_left <= 0.0 && ammo <= 0.0;
        let dist = dist3d(muzzle, lead_pos);
        let ready = w.cooldown <= 0.12 && reload_left <= 0.0 && ammo > 0.0 && dist <= range;
        let slew_ok = slew_aligned(
            cur_aim,
            aim,
            tgt.radius,
            dist.max(4.0),
            turn_rate,
            pitch_rate,
            1.15,
        );
        let los_ok = tgt.has_los && self.map.has_los(muzzle, lead_pos);

        (aim, ready && slew_ok && los_ok, need_reload)
    }

    fn eval_lightning(
        &self,
        cur_aim: [f64; 2],
        energy: f64,
        max_energy: f64,
        muzzle: [f64; 3],
        w: &WeaponObs,
        tgt: &TargetCandidate,
    ) -> ([f64; 2], bool) {
        let range = stat(&w.stats, "range", 45.0);
        let cone_rad = stat(&w.stats, "cone_deg", 30.0).to_radians();
        let e_cost = stat(&w.stats, "energy", 3.0);

        let aim = direct_aim(muzzle, [tgt.pos[0], tgt.pos[1], tgt.com_z]);
        let yaw_err = angle_diff(cur_aim[0], aim[0]).abs();
        let energy_ok =
            energy >= 0.25 * max_energy + e_cost || tgt.charging || tgt.hp < 300.0;
        let should_fire = tgt.has_los
            && tgt.dist <= range - 0.5
            && yaw_err <= 0.5 * cone_rad
            && w.cooldown <= 0.12
            && energy_ok;
        (aim, should_fire)
    }

    #[allow(clippy::too_many_arguments)]
    fn eval_laser(
        &self,
        cur_aim: [f64; 2],
        _me_pos: [f64; 3],
        muzzle: [f64; 3],
        w: &WeaponObs,
        tgt: &TargetCandidate,
        turn_rate: f64,
        pitch_rate: f64,
        min_pitch: f64,
        max_pitch: f64,
    ) -> ([f64; 2], bool) {
        let mut aim = direct_aim(muzzle, [tgt.pos[0], tgt.pos[1], tgt.com_z]);
        aim[1] = aim[1].clamp(min_pitch, max_pitch);

        let full_charge = stat(&w.stats, "charge", 0.75);
        let min_charge = stat(&w.stats, "min_charge", 0.15);
        let hold_s = stat(&w.stats, "hold_s", 2.0);
        let range = stat(&w.stats, "range", 280.0);
        let damage = stat(&w.stats, "damage", 315.0);
        let min_frac = stat(&w.stats, "min_frac", 0.35);
        let charge = w.charge.unwrap_or(0.0);

        let aligned = slew_aligned(
            cur_aim,
            aim,
            tgt.radius,
            tgt.dist.max(5.0),
            turn_rate,
            pitch_rate,
            0.85,
        );

        let should_hold = if charge <= 0.0 {
            // Start charging only when we have a mech target with LOS in range
            tgt.has_los
                && matches!(tgt.kind, TargetKind::Mech)
                && tgt.dist <= range - 2.0
                && w.cooldown <= 0.05
        } else if charge >= full_charge - 0.06 {
            // Release (`fire = false`) once aligned (or right before auto-fire hold_s expires)
            if tgt.has_los && aligned {
                false
            } else {
                charge < full_charge + hold_s - 0.25
            }
        } else if charge >= min_charge && tgt.has_los && aligned && tgt.hp <= damage * min_frac {
            // Quick finish on very low-HP target
            false
        } else {
            true
        };

        (aim, should_hold)
    }

    #[allow(clippy::too_many_arguments)]
    fn eval_mortar(
        &self,
        cur_aim: [f64; 2],
        me_pos: [f64; 3],
        mech_height: f64,
        w: &WeaponObs,
        tgt: &TargetCandidate,
        turn_rate: f64,
        pitch_rate: f64,
        min_pitch: f64,
        max_pitch: f64,
    ) -> Option<([f64; 2], bool)> {
        let (aim, pred_pos) =
            self.solve_mortar_aim(me_pos, mech_height, w, tgt, min_pitch, max_pitch)?;
        let dist = dist2d(me_pos[0], me_pos[1], pred_pos[0], pred_pos[1]);
        let splash_r = stat(&w.stats, "splash_radius", 7.0);
        let aligned = slew_aligned(
            cur_aim,
            aim,
            tgt.radius + splash_r * 0.4,
            dist.max(10.0),
            turn_rate,
            pitch_rate,
            1.1,
        );
        Some((aim, w.cooldown <= 0.12 && aligned))
    }

    fn solve_mortar_aim(
        &self,
        me_pos: [f64; 3],
        mech_height: f64,
        w: &WeaponObs,
        tgt: &TargetCandidate,
        min_pitch: f64,
        max_pitch: f64,
    ) -> Option<([f64; 2], [f64; 3])> {
        let speed = stat(&w.stats, "speed", 72.0);
        let g = stat(&w.stats, "gravity", 22.0);
        let ttl = stat(&w.stats, "ttl", 7.0);
        let splash_r = stat(&w.stats, "splash_radius", 7.0);
        let start = [me_pos[0], me_pos[1], me_pos[2] + mech_height + 0.5];

        let mut pred = [tgt.pos[0], tgt.pos[1], tgt.pos[2] + 1.0];
        let mut chosen_pitch = 0.0;
        let mut chosen_flight_t = 0.0;
        let mut found = false;

        for high_arc in [false, true] {
            pred = [tgt.pos[0], tgt.pos[1], tgt.pos[2] + 1.0];
            let mut ok = true;
            let mut flight_t = 0.0;
            let mut pitch = 0.0;

            for _ in 0..2 {
                let dx = pred[0] - start[0];
                let dy = pred[1] - start[1];
                let r = dx.hypot(dy);
                if r < splash_r + 10.0 {
                    ok = false;
                    break;
                }
                let dz = pred[2] - start[2];
                let u = speed * speed;
                let disc = u * u - g * (g * r * r + 2.0 * dz * u);
                if disc < 0.0 {
                    ok = false;
                    break;
                }
                let root = disc.sqrt();
                let tan_theta = if high_arc {
                    (u + root) / (g * r)
                } else {
                    (u - root) / (g * r)
                };
                pitch = tan_theta.atan();
                if pitch < min_pitch || pitch > max_pitch {
                    ok = false;
                    break;
                }
                let cos_p = pitch.cos();
                if cos_p < 1e-4 {
                    ok = false;
                    break;
                }
                flight_t = r / (speed * cos_p);
                if flight_t > ttl {
                    ok = false;
                    break;
                }
                pred = [
                    tgt.pos[0] + tgt.vel[0] * flight_t,
                    tgt.pos[1] + tgt.vel[1] * flight_t,
                    tgt.pos[2] + 1.0,
                ];
                let ground = self.map.ground_height(pred[0], pred[1]);
                if pred[2] < ground {
                    pred[2] = ground + 0.5;
                }
            }

            if ok && dist2d(me_pos[0], me_pos[1], pred[0], pred[1]) > splash_r + 10.0 {
                let yaw = (pred[1] - start[1]).atan2(pred[0] - start[0]);
                let launch_vel = [
                    speed * pitch.cos() * yaw.cos(),
                    speed * pitch.cos() * yaw.sin(),
                    speed * pitch.sin(),
                ];
                if self
                    .map
                    .has_mortar_arc_clearance(start, launch_vel, g, flight_t)
                {
                    chosen_pitch = pitch;
                    chosen_flight_t = flight_t;
                    found = true;
                    break;
                }
            }
        }

        let _ = chosen_flight_t;
        if found {
            let yaw = (pred[1] - start[1]).atan2(pred[0] - start[0]);
            Some(([yaw, chosen_pitch], pred))
        } else {
            None
        }
    }

    fn compute_goal_direction(
        &self,
        obs: &Observation,
        enemy: Option<TargetCandidate>,
    ) -> ([f64; 2], [f64; 2], bool, bool) {
        let me = &obs.self_obs;
        let nc = obs.zone.next_center;
        let dist_next = dist2d(me.pos[0], me.pos[1], nc[0], nc[1]);
        let safe_r = (obs.zone.next_radius - 18.0).max(0.0);
        let urgent_zone =
            !me.in_zone || (dist_next > safe_r && obs.t >= obs.zone.shrink_start - 18.0);

        if urgent_zone {
            let dir = unit2d(nc[0] - me.pos[0], nc[1] - me.pos[1]);
            return (dir, nc, true, false);
        }

        if let Some(e) = enemy {
            let to_e = unit2d(e.pos[0] - me.pos[0], e.pos[1] - me.pos[1]);
            let perp = [-to_e[1] * self.strafe_sign, to_e[0] * self.strafe_sign];

            let has_lightning = me
                .weapons
                .iter()
                .flatten()
                .any(|w| w.kind == WeaponKind::Lightning);
            let pref_dist = if has_lightning && me.energy > 30.0 && e.charging {
                28.0
            } else if me.hp < 0.45 * me.max_hp {
                95.0
            } else {
                62.0
            };

            let d2 = dist2d(me.pos[0], me.pos[1], e.pos[0], e.pos[1]);
            let radial_w = if d2 < pref_dist - 12.0 {
                -0.65
            } else if d2 > pref_dist + 20.0 {
                0.55
            } else {
                0.0
            };

            let mut gx = to_e[0] * radial_w + perp[0] * 0.85;
            let mut gy = to_e[1] * radial_w + perp[1] * 0.85;
            if dist_next > safe_r * 0.82 {
                let zdir = unit2d(nc[0] - me.pos[0], nc[1] - me.pos[1]);
                gx += zdir[0] * 0.55;
                gy += zdir[1] * 0.55;
            }
            let dir = unit2d(gx, gy);
            let goal_pt = [me.pos[0] + dir[0] * 20.0, me.pos[1] + dir[1] * 20.0];
            return (dir, goal_pt, false, false);
        }

        // Safe cube collection (<= 35 m inside next zone)
        if let Some(cube) = obs
            .cubes
            .iter()
            .filter(|c| {
                dist2d(me.pos[0], me.pos[1], c.pos[0], c.pos[1]) <= 35.0
                    && dist2d(c.pos[0], c.pos[1], nc[0], nc[1]) <= obs.zone.next_radius - 5.0
            })
            .min_by(|a, b| {
                dist2d(me.pos[0], me.pos[1], a.pos[0], a.pos[1])
                    .partial_cmp(&dist2d(me.pos[0], me.pos[1], b.pos[0], b.pos[1]))
                    .unwrap()
            })
        {
            let dir = unit2d(cube.pos[0] - me.pos[0], cube.pos[1] - me.pos[1]);
            return (dir, [cube.pos[0], cube.pos[1]], false, false);
        }

        // Empty slot 1 weapon pickup (<= 50 m inside next zone)
        if me.weapons.get(1).and_then(|w| w.as_ref()).is_none() {
            if let Some(pk) = obs
                .pickups
                .iter()
                .filter(|p| {
                    p.kind != WeaponKind::Gun
                        && !me.weapons.iter().flatten().any(|w| w.kind == p.kind)
                        && dist2d(me.pos[0], me.pos[1], p.pos[0], p.pos[1]) <= 50.0
                        && dist2d(p.pos[0], p.pos[1], nc[0], nc[1]) <= obs.zone.next_radius - 5.0
                })
                .min_by(|a, b| {
                    dist2d(me.pos[0], me.pos[1], a.pos[0], a.pos[1])
                        .partial_cmp(&dist2d(me.pos[0], me.pos[1], b.pos[0], b.pos[1]))
                        .unwrap()
                })
            {
                let dir = unit2d(pk.pos[0] - me.pos[0], pk.pos[1] - me.pos[1]);
                return (dir, [pk.pos[0], pk.pos[1]], false, false);
            }
        }

        // Move inside next safe ring
        if dist_next > safe_r * 0.65 {
            let dir = unit2d(nc[0] - me.pos[0], nc[1] - me.pos[1]);
            return (dir, nc, false, false);
        }

        // Safe NPC or crate farming when healthy
        if me.hp > 0.50 * me.max_hp && obs.t - self.last_damage_t > 3.0 {
            if let Some(npc) = obs
                .mechs
                .iter()
                .filter(|m| {
                    m.kind == "npc" && dist2d(me.pos[0], me.pos[1], m.pos[0], m.pos[1]) <= 75.0
                })
                .min_by(|a, b| {
                    dist2d(me.pos[0], me.pos[1], a.pos[0], a.pos[1])
                        .partial_cmp(&dist2d(me.pos[0], me.pos[1], b.pos[0], b.pos[1]))
                        .unwrap()
                })
            {
                let d = dist2d(me.pos[0], me.pos[1], npc.pos[0], npc.pos[1]);
                if d > 28.0 {
                    let dir = unit2d(npc.pos[0] - me.pos[0], npc.pos[1] - me.pos[1]);
                    return (dir, [npc.pos[0], npc.pos[1]], false, false);
                }
            }
            if let Some(cr) = obs
                .crates
                .iter()
                .filter(|c| dist2d(me.pos[0], me.pos[1], c.pos[0], c.pos[1]) <= 50.0)
                .min_by(|a, b| {
                    dist2d(me.pos[0], me.pos[1], a.pos[0], a.pos[1])
                        .partial_cmp(&dist2d(me.pos[0], me.pos[1], b.pos[0], b.pos[1]))
                        .unwrap()
                })
            {
                let d = dist2d(me.pos[0], me.pos[1], cr.pos[0], cr.pos[1]);
                if d > 20.0 {
                    let dir = unit2d(cr.pos[0] - me.pos[0], cr.pos[1] - me.pos[1]);
                    return (dir, [cr.pos[0], cr.pos[1]], false, false);
                }
            }
        }

        let dir = unit2d(nc[0] - me.pos[0], nc[1] - me.pos[1]);
        (dir, nc, false, false)
    }

    fn steer(
        &self,
        obs: &Observation,
        goal_dir: [f64; 2],
        mech_radius: f64,
        mech_height: f64,
    ) -> ([f64; 2], bool) {
        let me = &obs.self_obs;
        let step_up = stat(&me.stats, "step_up", 1.2);
        let max_slope_rad = stat(&me.stats, "max_slope_deg", 50.0).to_radians();
        let max_slope_tan = max_slope_rad.tan();
        let mortar_g = stat(&self.config.weapons.mortar, "gravity", 22.0);
        let splash_r = stat(&self.config.weapons.mortar, "splash_radius", 7.0);

        // Precompute incoming mortar impact points
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

        // Precompute charging laser lines to dodge perpendicularly
        let mut laser_threats: Vec<([f64; 2], [f64; 2])> = Vec::new();
        for m in &obs.mechs {
            if m.kind == "mech" && m.charging {
                let d = dist2d(me.pos[0], me.pos[1], m.pos[0], m.pos[1]);
                if d <= 240.0 {
                    let aim_dir = [m.aim[0].cos(), m.aim[0].sin()];
                    let to_me = unit2d(me.pos[0] - m.pos[0], me.pos[1] - m.pos[1]);
                    if aim_dir[0] * to_me[0] + aim_dir[1] * to_me[1] > 0.85 {
                        laser_threats.push(([m.pos[0], m.pos[1]], aim_dir));
                    }
                }
            }
        }

        let mut best_dir = goal_dir;
        let mut best_score = f64::NEG_INFINITY;
        let mut goal_blocked_near = false;

        for k in 0..16 {
            let theta = (k as f64) * (PI / 8.0);
            let dir = [theta.cos(), theta.sin()];
            let mut score = dir[0] * goal_dir[0] + dir[1] * goal_dir[1];
            score += 0.18 * (dir[0] * self.last_move[0] + dir[1] * self.last_move[1]);

            let check_step = |dist_m: f64| -> bool {
                let nx = me.pos[0] + dir[0] * dist_m;
                let ny = me.pos[1] + dir[1] * dist_m;
                let (ok, surf_z) = self.map.is_pos_walkable(
                    nx,
                    ny,
                    me.pos[2],
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
                if dir[0] * goal_dir[0] + dir[1] * goal_dir[1] > 0.85 {
                    goal_blocked_near = true;
                }
            } else if !ok_mid {
                score -= 1.8;
            } else if !ok_far {
                score -= 0.6;
            }

            for imp in &mortar_threats {
                let away = unit2d(me.pos[0] - imp[0], me.pos[1] - imp[1]);
                score += 2.2 * (dir[0] * away[0] + dir[1] * away[1]);
            }

            for (_, ray) in &laser_threats {
                let cross = (dir[0] * ray[1] - dir[1] * ray[0]).abs();
                score += 1.4 * cross;
            }

            if score > best_score {
                best_score = score;
                best_dir = dir;
            }
        }

        (best_dir, goal_blocked_near)
    }

    fn predict_mortar_impact(
        &self,
        pos: [f64; 3],
        vel: [f64; 3],
        g: f64,
    ) -> Option<[f64; 2]> {
        let mut t = 0.2;
        for _ in 0..14 {
            let x = pos[0] + vel[0] * t;
            let y = pos[1] + vel[1] * t;
            let z = pos[2] + vel[2] * t - 0.5 * g * t * t;
            let gh = self.map.ground_height(x, y);
            if z <= gh {
                return Some([x, y]);
            }
            t += 0.25;
        }
        None
    }
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

pub fn solve_gun_lead(
    muzzle: [f64; 3],
    target_pos: [f64; 3],
    target_vel: [f64; 3],
    bullet_speed: f64,
    shot_origin_offset: f64,
    min_pitch: f64,
    max_pitch: f64,
) -> ([f64; 2], [f64; 3]) {
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
            let t_pos1 = (-b - sq) / (2.0 * a);
            let t_pos2 = (-b + sq) / (2.0 * a);
            let mut best = f64::INFINITY;
            if t_pos1 > 0.0 {
                best = best.min(t_pos1);
            }
            if t_pos2 > 0.0 {
                best = best.min(t_pos2);
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
    let t1 = ((dist0 - shot_origin_offset).max(0.0) / bullet_speed.max(1.0)).min(2.5);
    let lead = [
        target_pos[0] + vx * t1,
        target_pos[1] + vy * t1,
        target_pos[2] + vz * t1,
    ];
    let mut aim = direct_aim(muzzle, lead);
    aim[1] = aim[1].clamp(min_pitch, max_pitch);
    (aim, lead)
}

#[inline]
fn slew_aligned(
    cur_aim: [f64; 2],
    target_aim: [f64; 2],
    target_radius: f64,
    dist: f64,
    turn_rate: f64,
    pitch_rate: f64,
    slack_mult: f64,
) -> bool {
    let ang_rad = (target_radius * slack_mult / dist.max(1.0))
        .atan()
        .clamp(0.02, 0.30);
    let yaw_err = angle_diff(cur_aim[0], target_aim[0]).abs();
    let pitch_err = (cur_aim[1] - target_aim[1]).abs();
    yaw_err <= ang_rad + 0.05 * turn_rate && pitch_err <= ang_rad + 0.05 * pitch_rate
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ServerMessage;
    use std::process::{Command, Stdio};

    #[test]
    fn lead_solver_intercepts_moving_target() {
        let muzzle = [0.0, 0.0, 7.0];
        let target_pos = [100.0, 0.0, 4.5];
        let target_vel = [0.0, 20.0, 0.0];
        let bullet_speed = 250.0;
        let offset = 3.0;
        let (aim, lead) = solve_gun_lead(muzzle, target_pos, target_vel, bullet_speed, offset, -0.7, 1.3);
        assert!(aim[0] > 0.05 && aim[0] < 0.12);
        assert!(lead[1] > 7.0 && lead[1] < 9.0);
        // Verify travel time matches lead time
        let dist = dist3d(muzzle, lead);
        let travel_t = (dist - offset) / bullet_speed;
        let expected_y = target_pos[1] + target_vel[1] * travel_t;
        assert!((lead[1] - expected_y).abs() < 0.15);
    }

    #[test]
    fn slew_gate_rejects_large_misalignment() {
        assert!(!slew_aligned([0.0, 0.0], [1.2, 0.0], 2.5, 60.0, 4.0, 3.0, 1.15));
        assert!(slew_aligned([0.0, 0.0], [0.05, 0.02], 2.5, 60.0, 4.0, 3.0, 1.15));
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

        // Deterministic rear-armor test: inject a Damaged event with bearing = -1.25
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

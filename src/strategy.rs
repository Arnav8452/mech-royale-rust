use std::f64::consts::PI;

use crate::combat::{weapon_desirability, TargetCandidate};
use crate::geom::{dist2d, is_ground_level, unit2d};
use crate::map::Map;
use crate::nav::{NavGrid, NavScratch};
use crate::protocol::{stat, Config, DropObs, Observation, Offer, SelfObs, WeaponKind};
use crate::world::WorldState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Posture {
    ZoneRotate,
    EmergencyRegen,
    ResetHeal,
    SupplyDrop,
    LootWeapon,
    Combat,
    EarlyFarm,
}

impl Posture {
    pub fn tag(self) -> &'static str {
        match self {
            Posture::ZoneRotate => "ZON",
            Posture::EmergencyRegen => "REG",
            Posture::ResetHeal => "RST",
            Posture::SupplyDrop => "DRP",
            Posture::LootWeapon => "LOT",
            Posture::Combat => "CMB",
            Posture::EarlyFarm => "FRM",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CombatTactic {
    None,
    CoverDuck,
    CornerPeek,
    AggressivePush,
    OpenStrafe,
    CoverAmbush,
}

impl CombatTactic {
    pub fn tag(self) -> &'static str {
        match self {
            CombatTactic::None => "",
            CombatTactic::CoverDuck => "DUCK",
            CombatTactic::CornerPeek => "PEEK",
            CombatTactic::AggressivePush => "PUSH",
            CombatTactic::OpenStrafe => "STRF",
            CombatTactic::CoverAmbush => "AMB",
        }
    }
}

pub(crate) struct StrategyState {
    pub posture: Posture,
    pub tactic: CombatTactic,
    pub peek_target: Option<[f64; 2]>,
    pub cached_path: Vec<[f64; 2]>,
    pub cached_path_goal: [f64; 2],
    pub cached_path_t: f64,
    pub cover_target: Option<[f64; 2]>,
    pub prev_cover_target: Option<[f64; 2]>,
    pub cover_target_t: f64,
    pub reset_cover_target: Option<[f64; 2]>,
    pub reset_cover_t: f64,
    pub combat_cover_target: Option<[f64; 2]>,
    pub combat_cover_t: f64,
    pub cover_hold_until: f64,
    pub ambush_anchor: Option<[f64; 2]>,
    pub ambush_anchor_t: f64,
    pub prev_ambush_anchor: Option<[f64; 2]>,
    pub explore_goal: Option<[f64; 2]>,
    pub explore_goal_t: f64,
}

impl StrategyState {
    pub fn new() -> Self {
        Self {
            posture: Posture::EarlyFarm,
            tactic: CombatTactic::None,
            peek_target: None,
            cached_path: Vec::new(),
            cached_path_goal: [0.0, 0.0],
            cached_path_t: -100.0,
            cover_target: None,
            prev_cover_target: None,
            cover_target_t: -100.0,
            reset_cover_target: None,
            reset_cover_t: -100.0,
            combat_cover_target: None,
            combat_cover_t: -100.0,
            cover_hold_until: -100.0,
            ambush_anchor: None,
            ambush_anchor_t: -100.0,
            prev_ambush_anchor: None,
            explore_goal: None,
            explore_goal_t: -100.0,
        }
    }

    pub fn pick_upgrade(&self, config: &Config, offer: &Offer, me: &SelfObs) -> usize {
        let mut best_idx = 0usize;
        let mut best_score = f64::NEG_INFINITY;
        let have_lightning = me
            .weapons
            .iter()
            .flatten()
            .any(|w| w.kind == WeaponKind::Lightning);

        for (i, choice) in offer.choices.iter().enumerate() {
            let score = if let Some(def) = config.upgrades.iter().find(|u| u.id == choice.id) {
                let held_req = def
                    .requires
                    .is_none_or(|req| me.weapons.iter().flatten().any(|w| w.kind == req));
                let existing_stacks = me.upgrades.iter().filter(|u| *u == &def.id).count();

                let mut max_eff = 10.0_f64;
                for eff in &def.effects {
                    let base = match eff.path.as_str() {
                        "mech.regen_per_s" | "mech.regen_delay" => 114.0,
                        "mech.hp" => 108.0,
                        "mech.armor" => 106.0,
                        "weapons.lightning.range" | "weapons.lightning.cone_deg" => 104.0,
                        "weapons.lightning.damage" | "weapons.lightning.energy" => 103.0,
                        "weapons.laser.damage" | "weapons.laser.charge" | "weapons.laser.pierce" => {
                            100.0
                        }
                        "weapons.gun.damage" | "weapons.gun.cooldown" => 98.0,
                        "weapons.gun.mag" | "weapons.gun.reload" => 96.0,
                        "weapons.lightning.chain" => 95.0,
                        "mech.walk_speed" | "mech.boost_speed" => 89.0,
                        "weapons.gun.speed" | "weapons.gun.range" => 86.0,
                        "mech.energy_regen" | "mech.boost_drain" | "mech.jet_drain" => {
                            if have_lightning {
                                84.0
                            } else {
                                76.0
                            }
                        }
                        "mech.energy" => {
                            if have_lightning {
                                80.0
                            } else {
                                68.0
                            }
                        }
                        "mech.near_range_bonus" | "mech.vision_bonus" | "mech.hearing" => {
                            if existing_stacks == 0 {
                                72.0
                            } else {
                                58.0
                            }
                        }
                        "mech.turn_rate" | "mech.pitch_rate" => 70.0,
                        p if p.starts_with("weapons.mortar.") => 30.0,
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

    pub fn pick_weapon_take_and_drop(
        &self,
        config: &Config,
        world: &WorldState,
        obs: &Observation,
        los_mechs: &[&TargetCandidate],
    ) -> (Option<u64>, Option<usize>) {
        let me = &obs.self_obs;
        let reach = (config.loot.pickup_radius - 0.1).max(0.5);
        let empty_slot = me.weapons.iter().take(2).position(|w| w.is_none());

        let mut best_pickup: Option<(u8, f64, WeaponKind, u64)> = None;
        for p in &obs.pickups {
            if p.kind == WeaponKind::Mortar || world.blacklisted_pickups.contains_key(&p.id) {
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

        let Some((_prio, _, pk_kind, pid)) = best_pickup else {
            return (None, None);
        };

        if empty_slot.is_some() {
            return (Some(pid), None);
        }

        // Swapping when both slots full: never swap under close LOS fire or charging laser
        let close_threat = los_mechs.iter().any(|m| m.dist < 65.0);
        let laser_busy = me
            .weapons
            .iter()
            .flatten()
            .any(|w| w.kind == WeaponKind::Laser && w.charge.unwrap_or(0.0) > 0.0);
        if close_threat || laser_busy {
            return (None, None);
        }

        if let Some(drop_slot) = swap_slot_for(me, pk_kind, obs.alive) {
            return (Some(pid), Some(drop_slot));
        }
        (None, None)
    }

    pub fn ensure_regen_cover(
        &mut self,
        map: &Map,
        nav: &NavGrid,
        world: &WorldState,
        obs: &Observation,
        eye_h: f64,
        los_mech_count: usize,
    ) -> Option<[f64; 2]> {
        let me = &obs.self_obs;
        let mut tracked_sorted: Vec<(u64, &crate::world::TrackedEnemy)> = world
            .tracked_enemies
            .iter()
            .filter(|(_, e)| dist2d(me.pos[0], me.pos[1], e.pos[0], e.pos[1]) <= 220.0)
            .map(|(&id, e)| (id, e))
            .collect();
        tracked_sorted.sort_by_key(|(id, _)| *id);
        let threats: Vec<[f64; 3]> = tracked_sorted
            .iter()
            .map(|(_, e)| [e.pos[0], e.pos[1], e.pos[2] + 7.5])
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
            self.cover_target = nav.find_cover_point_within(
                map,
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

    #[allow(clippy::too_many_arguments)]
    pub fn update_posture(
        &mut self,
        map: &Map,
        nav: &NavGrid,
        world: &WorldState,
        config: &Config,
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
        let stat_cap = config.cubes.stat_cap.round() as u64;
        let slot1_empty = me.weapons.get(1).and_then(|w| w.as_ref()).is_none();

        if !me.in_zone || (dist_next > safe_r && obs.t >= obs.zone.shrink_start - 20.0) {
            self.posture = Posture::ZoneRotate;
            return;
        }

        let winning_or_even_1v1 = los_count == 1 && los_mechs[0].hp <= me.hp + 180.0;
        let enemy_in_face = los_mechs.iter().any(|m| m.dist < 32.0);
        let regen_exit = if near_mechs > 0 { 0.78 } else { 0.72 };
        let want_regen = if self.posture == Posture::EmergencyRegen {
            !immediate_kill_avail
                && !enemy_in_face
                && me.hp < regen_exit * me.max_hp
                && !(me.hp >= 0.65 * me.max_hp && near_mechs == 0)
        } else {
            !immediate_kill_avail
                && !enemy_in_face
                && ((me.hp < 0.50 * me.max_hp && los_count >= 2)
                    || (me.hp < 0.30 * me.max_hp && near_mechs > 0 && !winning_or_even_1v1))
        };
        if want_regen
            && (self
                .ensure_regen_cover(map, nav, world, obs, eye_h, los_count)
                .is_some()
                || (self.posture == Posture::EmergencyRegen && los_count == 0))
        {
            self.posture = Posture::EmergencyRegen;
            return;
        }

        if self.find_best_supply_drop(world, obs, los_mechs).is_some() {
            self.posture = Posture::SupplyDrop;
            return;
        }

        let weapon_goal = self.find_best_weapon_goal(map, world, obs, los_mechs);
        if slot1_empty && me.hp >= 0.45 * me.max_hp && weapon_goal.is_some() {
            self.posture = Posture::LootWeapon;
            return;
        }

        // Silent between-kill full heal: hide out of LOS and wait quietly up to 96% HP
        let post_kill_cube = obs.t - world.last_kill_t <= 2.5
            && me.cubes < stat_cap
            && obs.cubes.iter().any(|c| {
                is_ground_level(map, c.pos)
                    && dist2d(me.pos[0], me.pos[1], c.pos[0], c.pos[1]) <= 22.0
            });
        let want_reset = if self.posture == Posture::ResetHeal {
            me.hp < 0.96 * me.max_hp
        } else {
            me.hp < 0.88 * me.max_hp
        };
        if los_count == 0 && want_reset && !post_kill_cube {
            self.posture = Posture::ResetHeal;
            return;
        }

        if weapon_goal.is_some() {
            self.posture = Posture::LootWeapon;
            return;
        }

        if world
            .tracked_enemies
            .values()
            .any(|e| dist2d(me.pos[0], me.pos[1], e.pos[0], e.pos[1]) <= 220.0)
        {
            self.posture = Posture::Combat;
            return;
        }

        self.posture = Posture::EarlyFarm;
    }

    #[allow(clippy::too_many_arguments)]
    pub fn compute_nav_goal(
        &mut self,
        map: &Map,
        nav: &NavGrid,
        scratch: &mut NavScratch,
        world: &mut WorldState,
        config: &Config,
        obs: &Observation,
        eye_h: f64,
        mech_cands: &[TargetCandidate],
        los_mechs: &[&TargetCandidate],
        combat_enemy: Option<&TargetCandidate>,
        farm_target: Option<&TargetCandidate>,
        immediate_kill_avail: bool,
        stuck_active: bool,
    ) -> ([f64; 2], [f64; 2]) {
        self.tactic = CombatTactic::None;
        self.peek_target = None;
        let was_orbiting = world.orbit_active_1v1;
        world.orbit_active_1v1 = false;

        let me = &obs.self_obs;
        let stat_cap = config.cubes.stat_cap.round() as u64;
        let want_cubes = me.cubes < stat_cap || (me.cubes >= 13 && me.cubes < 15);
        let slot1_empty = me.weapons.get(1).and_then(|w| w.as_ref()).is_none();
        let my_lt_range = me
            .weapons
            .iter()
            .flatten()
            .find(|w| w.kind == WeaponKind::Lightning)
            .map(|w| stat(&w.stats, "range", 45.0))
            .unwrap_or(45.0);
        let los_mech_count = los_mechs.len();
        let me_xy = [me.pos[0], me.pos[1]];
        let nc = obs.zone.next_center;
        let safe_r = (obs.zone.next_radius - 18.0).max(0.0);
        let dist_next = dist2d(me.pos[0], me.pos[1], nc[0], nc[1]);

        match self.posture {
            Posture::ZoneRotate => {
                let wp = self.follow_astar(nav, scratch, me_xy, nc, obs.t);
                return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), nc);
            }
            Posture::EmergencyRegen => {
                if let Some(cp) = self.ensure_regen_cover(map, nav, world, obs, eye_h, los_mech_count)
                {
                    if dist2d(me.pos[0], me.pos[1], cp[0], cp[1]) <= 5.0 && los_mech_count == 0 {
                        return ([0.0, 0.0], cp);
                    }
                    let wp = self.follow_astar(nav, scratch, me_xy, cp, obs.t);
                    return (
                        zigzag_cover_dir(me.pos, wp, cp, los_mech_count, world.strafe_sign),
                        cp,
                    );
                }
                if let Some((_, th)) = world.closest_tracked_enemy_within(me.pos, 220.0) {
                    let away = unit2d(me.pos[0] - th.pos[0], me.pos[1] - th.pos[1]);
                    let zdir = unit2d(nc[0] - me.pos[0], nc[1] - me.pos[1]);
                    let goal = [
                        me.pos[0] + (away[0] * 0.75 + zdir[0] * 0.35) * 40.0,
                        me.pos[1] + (away[1] * 0.75 + zdir[1] * 0.35) * 40.0,
                    ];
                    let wp = self.follow_astar(nav, scratch, me_xy, goal, obs.t);
                    return (
                        zigzag_cover_dir(me.pos, wp, goal, los_mech_count, world.strafe_sign),
                        goal,
                    );
                }
                return (unit2d(nc[0] - me.pos[0], nc[1] - me.pos[1]), nc);
            }
            Posture::ResetHeal => {
                if want_cubes && obs.t - world.last_kill_t <= 3.0 {
                    if let Some(cube) = obs
                        .cubes
                        .iter()
                        .filter(|c| {
                            is_ground_level(map, c.pos)
                                && dist2d(me.pos[0], me.pos[1], c.pos[0], c.pos[1]) <= 18.0
                                && dist2d(c.pos[0], c.pos[1], nc[0], nc[1])
                                    <= obs.zone.next_radius - 5.0
                        })
                        .min_by(|a, b| {
                            let da = dist2d(me.pos[0], me.pos[1], a.pos[0], a.pos[1]);
                            let db = dist2d(me.pos[0], me.pos[1], b.pos[0], b.pos[1]);
                            da.total_cmp(&db).then_with(|| a.id.cmp(&b.id))
                        })
                    {
                        let goal = [cube.pos[0], cube.pos[1]];
                        let wp = self.follow_astar(nav, scratch, me_xy, goal, obs.t);
                        return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), goal);
                    }
                }

                let mut tracked_sorted: Vec<(u64, &crate::world::TrackedEnemy)> = world
                    .tracked_enemies
                    .iter()
                    .filter(|(_, e)| dist2d(me.pos[0], me.pos[1], e.pos[0], e.pos[1]) <= 220.0)
                    .map(|(&id, e)| (id, e))
                    .collect();
                tracked_sorted.sort_by_key(|(id, _)| *id);
                let threats: Vec<[f64; 3]> = tracked_sorted
                    .iter()
                    .map(|(_, e)| [e.pos[0], e.pos[1], e.pos[2] + 7.5])
                    .collect();

                if !threats.is_empty() {
                    let keep = self.reset_cover_target.is_some_and(|cp| {
                        obs.t - self.reset_cover_t < 2.5
                            && dist2d(cp[0], cp[1], nc[0], nc[1])
                                <= (obs.zone.next_radius - 8.0).max(0.0)
                    });
                    if !keep {
                        self.reset_cover_target = nav.find_cover_point_within(
                            map,
                            me.pos,
                            eye_h,
                            &threats,
                            obs.zone.center,
                            obs.zone.radius,
                            obs.zone.next_center,
                            obs.zone.next_radius,
                            52.0,
                            None,
                        );
                        self.reset_cover_t = obs.t;
                    }
                } else {
                    let keep = self.reset_cover_target.is_some_and(|cp| {
                        obs.t - self.reset_cover_t < 5.0
                            && dist2d(cp[0], cp[1], nc[0], nc[1])
                                <= (obs.zone.next_radius - 10.0).max(0.0)
                    });
                    if !keep {
                        self.reset_cover_target = nav.find_cover_anchor(
                            me.pos,
                            obs.zone.center,
                            obs.zone.radius,
                            nc,
                            obs.zone.next_radius,
                            None,
                            36.0,
                        );
                        self.reset_cover_t = obs.t;
                    }
                }

                if let Some(cp) = self.reset_cover_target {
                    if dist2d(me.pos[0], me.pos[1], cp[0], cp[1]) <= 5.5 {
                        return ([0.0, 0.0], cp);
                    }
                    let wp = self.follow_astar(nav, scratch, me_xy, cp, obs.t);
                    return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), cp);
                }
                if dist_next > safe_r * 0.75 {
                    let wp = self.follow_astar(nav, scratch, me_xy, nc, obs.t);
                    return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), nc);
                }
                return ([0.0, 0.0], me_xy);
            }
            Posture::SupplyDrop => {
                if let Some(drop) = self.find_best_supply_drop(world, obs, los_mechs) {
                    let d = dist2d(me.pos[0], me.pos[1], drop.pos[0], drop.pos[1]);
                    if drop.lands_in > 5.0 && d < 28.0 {
                        let threats: Vec<[f64; 3]> = mech_cands
                            .iter()
                            .filter(|m| m.has_los)
                            .map(|m| [m.pos[0], m.pos[1], m.pos[2] + 7.5])
                            .collect();
                        if !threats.is_empty() {
                            if let Some(cp) = nav.find_cover_point(
                                map,
                                me.pos,
                                eye_h,
                                &threats,
                                obs.zone.center,
                                obs.zone.radius,
                                obs.zone.next_center,
                                obs.zone.next_radius,
                            ) {
                                let wp = self.follow_astar(nav, scratch, me_xy, cp, obs.t);
                                return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), cp);
                            }
                        }
                        let ang = obs.t * 1.1 * world.strafe_sign;
                        let orbit = [
                            drop.pos[0] + 16.0 * ang.cos(),
                            drop.pos[1] + 16.0 * ang.sin(),
                        ];
                        return (unit2d(orbit[0] - me.pos[0], orbit[1] - me.pos[1]), orbit);
                    }
                    if drop.lands_in <= 4.5 && d <= (drop.radius - 1.5).max(1.0) {
                        let r_in = (drop.radius * 0.55).max(1.5);
                        let ang = obs.t * 1.8 * world.strafe_sign;
                        let pt = [
                            drop.pos[0] + r_in * ang.cos(),
                            drop.pos[1] + r_in * ang.sin(),
                        ];
                        return (unit2d(pt[0] - me.pos[0], pt[1] - me.pos[1]), pt);
                    }
                    let wp =
                        self.follow_astar(nav, scratch, me_xy, [drop.pos[0], drop.pos[1]], obs.t);
                    return (
                        unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]),
                        [drop.pos[0], drop.pos[1]],
                    );
                }
            }
            Posture::LootWeapon => {
                if let Some(pk_pos) = self.find_best_weapon_goal(map, world, obs, los_mechs) {
                    let wp = self.follow_astar(nav, scratch, me_xy, pk_pos, obs.t);
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
                let our_laser_charge_frac = me
                    .weapons
                    .iter()
                    .flatten()
                    .find(|w| w.kind == WeaponKind::Laser)
                    .map(|w| {
                        let full = stat(&w.stats, "charge", 0.75).max(0.01);
                        w.charge.unwrap_or(0.0) / full
                    })
                    .unwrap_or(0.0);

                let lt_ready_for_charger = me.weapons.iter().flatten().any(|w| {
                    w.kind == WeaponKind::Lightning
                        && me.energy >= 12.0
                        && los_mechs.iter().any(|m| m.charging && m.dist <= my_lt_range)
                });
                let enemy_laser_duck = !immediate_kill_avail
                    && !lt_ready_for_charger
                    && our_laser_charge_frac < 0.65
                    && los_mechs.iter().any(|e| e.charging);

                let near_cover_14 = nav.has_near_cover_within(me.pos, 14.0);
                let has_lightning = me
                    .weapons
                    .iter()
                    .flatten()
                    .any(|w| w.kind == WeaponKind::Lightning);
                if los_mech_count >= 2 {
                    self.cover_hold_until = obs.t + 1.1;
                }
                let recent_multi_cover = obs.t < self.cover_hold_until
                    && world
                        .tracked_enemies
                        .values()
                        .filter(|te| dist2d(me.pos[0], me.pos[1], te.pos[0], te.pos[1]) <= 180.0)
                        .count()
                        >= 2;
                let laser_cd_duck = los_mech_count == 1
                    && have_laser
                    && !immediate_kill_avail
                    && near_cover_14
                    && combat_enemy.is_some_and(|e| {
                        !e.is_vulnerable
                            && e.hp > 480.0
                            && e.dist > 45.0
                            && !(has_lightning && e.dist <= my_lt_range + 4.0)
                    })
                    && me.weapons.iter().flatten().any(|w| {
                        w.kind == WeaponKind::Laser
                            && w.cooldown > 0.35
                            && w.charge.unwrap_or(0.0) <= 0.0
                    });

                // A. Mid-Combat Power Cube Magnet: free +4% max HP heal & +4% dmg mid-fight (gated on want_cubes)
                if want_cubes
                    && !enemy_laser_duck
                    && los_mech_count <= 1
                    && !(no_weapon_can_shoot_now && los_mech_count >= 1)
                {
                    let cube_pull_r = if los_mech_count == 0 { 24.0 } else { 16.0 };
                    let have_lt_ready = me
                        .weapons
                        .iter()
                        .flatten()
                        .any(|w| w.kind == WeaponKind::Lightning)
                        && me.energy > 25.0;
                    if let Some(cube) = obs
                        .cubes
                        .iter()
                        .filter(|c| {
                            is_ground_level(map, c.pos)
                                && dist2d(me.pos[0], me.pos[1], c.pos[0], c.pos[1]) <= cube_pull_r
                                && dist2d(c.pos[0], c.pos[1], nc[0], nc[1])
                                    <= obs.zone.next_radius - 5.0
                                && !(los_mech_count == 1
                                    && !have_lt_ready
                                    && los_mechs[0].hp > 480.0
                                    && dist2d(
                                        c.pos[0],
                                        c.pos[1],
                                        los_mechs[0].pos[0],
                                        los_mechs[0].pos[1],
                                    ) < 22.0)
                        })
                        .min_by(|a, b| {
                            dist2d(me.pos[0], me.pos[1], a.pos[0], a.pos[1])
                                .partial_cmp(&dist2d(me.pos[0], me.pos[1], b.pos[0], b.pos[1]))
                                .unwrap()
                        })
                    {
                        self.tactic = CombatTactic::OpenStrafe;
                        let goal = [cube.pos[0], cube.pos[1]];
                        let wp = self.follow_astar(nav, scratch, me_xy, goal, obs.t);
                        return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), goal);
                    }
                }

                // 1. CoverDuck: outnumbered (2+ LOS / hold hysteresis), dry/reloading, enemy charging laser, or Laser cooldown peek-cycle
                if ((los_mech_count >= 2 || recent_multi_cover || no_weapon_can_shoot_now)
                    && !immediate_kill_avail)
                    || enemy_laser_duck
                    || laser_cd_duck
                {
                    if los_mech_count == 0
                        && (near_cover_14
                            || self.combat_cover_target.is_some_and(|cp| {
                                dist2d(me.pos[0], me.pos[1], cp[0], cp[1]) <= 6.5
                            }))
                    {
                        self.tactic = CombatTactic::CoverDuck;
                        let anchor = self.combat_cover_target.unwrap_or(me_xy);
                        return ([0.0, 0.0], anchor);
                    }

                    let mut threats: Vec<[f64; 3]> = mech_cands
                        .iter()
                        .filter(|m| m.has_los)
                        .map(|m| [m.pos[0], m.pos[1], m.pos[2] + 7.5])
                        .collect();
                    if threats.is_empty() {
                        let mut tracked_sorted: Vec<(u64, &crate::world::TrackedEnemy)> = world
                            .tracked_enemies
                            .iter()
                            .filter(|(_, e)| {
                                dist2d(me.pos[0], me.pos[1], e.pos[0], e.pos[1]) <= 220.0
                            })
                            .map(|(&id, e)| (id, e))
                            .collect();
                        tracked_sorted.sort_by_key(|(id, _)| *id);
                        threats = tracked_sorted
                            .iter()
                            .map(|(_, e)| [e.pos[0], e.pos[1], e.pos[2] + 7.5])
                            .collect();
                    }

                    if !threats.is_empty() {
                        let arrived_exposed = self.combat_cover_target.is_some_and(|cp| {
                            dist2d(me.pos[0], me.pos[1], cp[0], cp[1]) <= 5.5 && los_mech_count > 0
                        });
                        if arrived_exposed {
                            self.combat_cover_target = None;
                        }
                        let keep = self
                            .combat_cover_target
                            .is_some_and(|_| obs.t - self.combat_cover_t < 1.2);
                        let cover_search_r = if los_mech_count >= 2 { 68.0 } else { 48.0 };
                        if !keep {
                            self.combat_cover_target = nav.find_cover_point_within(
                                map,
                                me.pos,
                                eye_h,
                                &threats,
                                obs.zone.center,
                                obs.zone.radius,
                                obs.zone.next_center,
                                obs.zone.next_radius,
                                cover_search_r,
                                None,
                            );
                            self.combat_cover_t = obs.t;
                        }
                        if let Some(cp) = self.combat_cover_target {
                            self.tactic = CombatTactic::CoverDuck;
                            if los_mech_count == 0
                                && dist2d(me.pos[0], me.pos[1], cp[0], cp[1]) <= 5.0
                            {
                                return ([0.0, 0.0], cp);
                            }
                            let wp = self.follow_astar(nav, scratch, me_xy, cp, obs.t);
                            return (
                                zigzag_cover_dir(me.pos, wp, cp, los_mech_count, world.strafe_sign),
                                cp,
                            );
                        }
                    }

                    // Open-pack escape: if 2+ LOS enemies have no cover within 68 m, kite away from pack centroid toward safety
                    if los_mech_count >= 2 {
                        self.tactic = CombatTactic::CoverDuck;
                        let (mut sx, mut sy) = (0.0_f64, 0.0_f64);
                        for m in los_mechs {
                            sx += m.pos[0];
                            sy += m.pos[1];
                        }
                        let n = los_mech_count as f64;
                        let away = unit2d(me.pos[0] - sx / n, me.pos[1] - sy / n);
                        let safe_pt = nav
                            .find_cover_anchor(
                                me.pos,
                                obs.zone.center,
                                obs.zone.radius,
                                nc,
                                obs.zone.next_radius,
                                None,
                                95.0,
                            )
                            .unwrap_or(nc);
                        let safe_dir = unit2d(safe_pt[0] - me.pos[0], safe_pt[1] - me.pos[1]);
                        let goal = [
                            me.pos[0] + (away[0] * 0.65 + safe_dir[0] * 0.35) * 45.0,
                            me.pos[1] + (away[1] * 0.65 + safe_dir[1] * 0.35) * 45.0,
                        ];
                        let wp = self.follow_astar(nav, scratch, me_xy, goal, obs.t);
                        return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), goal);
                    }
                }

                if let Some(e) = combat_enemy {
                    // 2. Enemy behind wall (!e.has_los): slice-the-pie corner peek (0 exposed other) or corner hold — never yo-yo into 2+ threats!
                    if !e.has_los {
                        let mut others: Vec<(u64, &crate::world::TrackedEnemy)> = world
                            .tracked_enemies
                            .iter()
                            .filter(|(&id, te)| {
                                id != e.id
                                    && dist2d(me.pos[0], me.pos[1], te.pos[0], te.pos[1]) <= 180.0
                            })
                            .map(|(&id, te)| (id, te))
                            .collect();
                        others.sort_by_key(|(id, _)| *id);
                        let close_other_threats: Vec<[f64; 3]> = others
                            .iter()
                            .map(|(_, te)| [te.pos[0], te.pos[1], te.pos[2] + 7.5])
                            .collect();

                        let target_eye = [e.pos[0], e.pos[1], e.pos[2] + 7.5];
                        if let Some(pp) = nav.find_peek_point(
                            map,
                            me.pos,
                            eye_h,
                            target_eye,
                            &close_other_threats,
                            obs.zone.center,
                            obs.zone.radius,
                            18.0,
                        ) {
                            self.tactic = CombatTactic::CornerPeek;
                            self.peek_target = Some(pp);
                            let wp = self.follow_astar(nav, scratch, me_xy, pp, obs.t);
                            return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), pp);
                        } else if close_other_threats.is_empty()
                            && (e.hp <= 550.0
                                || e.is_vulnerable
                                || me.hp >= 0.55 * me.max_hp
                                || immediate_kill_avail)
                        {
                            self.tactic = CombatTactic::AggressivePush;
                            let goal = [e.pos[0], e.pos[1]];
                            let wp = self.follow_astar(nav, scratch, me_xy, goal, obs.t);
                            return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), goal);
                        }

                        self.tactic = CombatTactic::CoverAmbush;
                        if dist_next > safe_r * 0.85 {
                            let wp = self.follow_astar(nav, scratch, me_xy, nc, obs.t);
                            return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), nc);
                        }
                        return ([0.0, 0.0], me_xy);
                    }

                    // 3. Direct engagement: continuous 1v1 orbit on any single LOS enemy
                    let to_e = unit2d(e.pos[0] - me.pos[0], e.pos[1] - me.pos[1]);
                    let is_1v1 = los_mech_count == 1;

                    let (perp, radial_band, tang_w) = if is_1v1 {
                        world.orbit_active_1v1 = true;
                        let orbit_blocked_for = |sign: f64| -> bool {
                            let p = [-to_e[1] * sign, to_e[0] * sign];
                            let mut blocked = stuck_active;
                            for step_m in [8.0_f64, 13.0_f64] {
                                let px = me.pos[0] + p[0] * step_m;
                                let py = me.pos[1] + p[1] * step_m;
                                let (cx, cy) = nav.world_to_cell(px, py);
                                if !nav.walkable[cy * nav.nx + cx] {
                                    blocked = true;
                                    break;
                                }
                            }
                            let p12 = [me.pos[0] + p[0] * 12.0, me.pos[1] + p[1] * 12.0];
                            let d_p12_nc = dist2d(p12[0], p12[1], nc[0], nc[1]);
                            let leaves_zone = obs.zone.next_radius > 20.0
                                && d_p12_nc > (obs.zone.next_radius - 14.0).max(0.0)
                                && d_p12_nc > dist_next;
                            let probe_eye = [
                                me.pos[0] + p[0] * 10.0,
                                me.pos[1] + p[1] * 10.0,
                                me.pos[2] + eye_h,
                            ];
                            let exposes_other = world.tracked_enemies.iter().any(|(&id, te)| {
                                id != e.id
                                    && map.has_los(
                                        probe_eye,
                                        [te.pos[0], te.pos[1], te.pos[2] + eye_h],
                                    )
                            });
                            blocked || leaves_zone || exposes_other
                        };

                        if !was_orbiting {
                            let perp_pos = [-to_e[1], to_e[0]];
                            let pref_dir = if let Some((hpos, _)) = world.last_hunt_pos {
                                unit2d(me.pos[0] - hpos[0], me.pos[1] - hpos[1])
                            } else {
                                unit2d(nc[0] - me.pos[0], nc[1] - me.pos[1])
                            };
                            let mut init_sign =
                                if perp_pos[0] * pref_dir[0] + perp_pos[1] * pref_dir[1] >= 0.0 {
                                    1.0
                                } else {
                                    -1.0
                                };
                            if orbit_blocked_for(init_sign) && !orbit_blocked_for(-init_sign) {
                                init_sign = -init_sign;
                            }
                            world.orbit_sign = init_sign;
                        } else if orbit_blocked_for(world.orbit_sign)
                            && !orbit_blocked_for(-world.orbit_sign)
                            && obs.t - world.last_orbit_flip_t >= 0.45
                        {
                            world.orbit_sign = -world.orbit_sign;
                            world.last_orbit_flip_t = obs.t;
                        }
                        (
                            [-to_e[1] * world.orbit_sign, to_e[0] * world.orbit_sign],
                            6.0_f64,
                            1.0_f64,
                        )
                    } else {
                        (
                            [-to_e[1] * world.strafe_sign, to_e[0] * world.strafe_sign],
                            14.0_f64,
                            0.85_f64,
                        )
                    };
                    let has_lightning = me
                        .weapons
                        .iter()
                        .flatten()
                        .any(|w| w.kind == WeaponKind::Lightning);
                    let enemy_has_lightning = e.weapons.contains(&WeaponKind::Lightning);
                    let enemy_has_laser = e.weapons.contains(&WeaponKind::Laser);
                    let base_pref_dist: f64 = if has_lightning
                        && me.energy > 20.0
                        && los_mech_count == 1
                        && me.hp > 0.30 * me.max_hp
                        && (!have_laser || e.dist <= 80.0 || e.charging)
                    {
                        if enemy_has_lightning && my_lt_range >= 50.0 {
                            (my_lt_range - 5.0).max(47.0)
                        } else {
                            (my_lt_range - 15.0).max(30.0)
                        }
                    } else if enemy_has_lightning && !has_lightning {
                        54.0
                    } else if enemy_has_laser && !have_laser {
                        42.0
                    } else if me.hp < 0.40 * me.max_hp {
                        68.0
                    } else {
                        44.0
                    };

                    let d2 = dist2d(me.pos[0], me.pos[1], e.pos[0], e.pos[1]);
                    let (pref_dist, push_w) = if e.is_vulnerable || e.hp <= 480.0 {
                        self.tactic = CombatTactic::AggressivePush;
                        ((base_pref_dist - 10.0).max(22.0), 0.78)
                    } else if has_lightning
                        && me.energy > 20.0
                        && d2 > (my_lt_range - 3.0).max(36.0)
                        && base_pref_dist <= my_lt_range - 5.0
                    {
                        self.tactic = CombatTactic::AggressivePush;
                        (base_pref_dist, 0.80)
                    } else {
                        self.tactic = CombatTactic::OpenStrafe;
                        (base_pref_dist, 0.65)
                    };

                    let in_band_neg = if is_1v1 { 6.0 } else { 10.0 };
                    let radial_w = if d2 < pref_dist - in_band_neg {
                        -0.65
                    } else if d2 > pref_dist + if self.tactic == CombatTactic::AggressivePush {
                        8.0
                    } else {
                        radial_band
                    } {
                        push_w
                    } else {
                        0.0
                    };
                    let mut gx = to_e[0] * radial_w + perp[0] * tang_w;
                    let mut gy = to_e[1] * radial_w + perp[1] * tang_w;

                    if !(has_lightning && d2 <= my_lt_range + 4.0 && me.energy > 20.0)
                        && (!e.is_vulnerable || los_mech_count >= 2)
                    {
                        if let Some(corner) = nav.find_cover_anchor(
                            me.pos,
                            obs.zone.center,
                            obs.zone.radius,
                            nc,
                            obs.zone.next_radius,
                            None,
                            32.0,
                        ) {
                            if dist2d(me.pos[0], me.pos[1], corner[0], corner[1]) > 8.5 {
                                let cdir = unit2d(corner[0] - me.pos[0], corner[1] - me.pos[1]);
                                let corner_w = if enemy_has_laser || have_laser {
                                    0.75
                                } else {
                                    0.45
                                };
                                gx += cdir[0] * corner_w;
                                gy += cdir[1] * corner_w;
                            }
                        }
                    }

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

        // EarlyFarm / fallback: cubes -> farm_target -> crates -> zone -> near sounds -> 50 m unvisited snowball -> 15 s hunt -> mid/far sounds -> sweep
        let regen_delay = stat(&me.stats, "regen_delay", 5.0);
        let protecting_regen = me.hp < 0.72 * me.max_hp
            && (obs.t - world.last_combat_t) >= 2.0
            && (obs.t - world.last_combat_t) <= regen_delay + 12.0;

        if want_cubes {
            if let Some(cube) = obs
                .cubes
                .iter()
                .filter(|c| {
                    is_ground_level(map, c.pos)
                        && dist2d(me.pos[0], me.pos[1], c.pos[0], c.pos[1]) <= 85.0
                        && dist2d(c.pos[0], c.pos[1], nc[0], nc[1]) <= obs.zone.next_radius - 5.0
                })
                .min_by(|a, b| {
                    dist2d(me.pos[0], me.pos[1], a.pos[0], a.pos[1])
                        .partial_cmp(&dist2d(me.pos[0], me.pos[1], b.pos[0], b.pos[1]))
                        .unwrap()
                })
            {
                let goal = [cube.pos[0], cube.pos[1]];
                let wp = self.follow_astar(nav, scratch, me_xy, goal, obs.t);
                return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), goal);
            }
        }

        if let Some(ft) = farm_target {
            let goal = [ft.pos[0], ft.pos[1]];
            let d = dist2d(me.pos[0], me.pos[1], goal[0], goal[1]);
            let stop_d = if matches!(ft.kind, crate::combat::TargetKind::Npc) {
                26.0
            } else {
                18.0
            };
            if d > stop_d {
                let wp = self.follow_astar(nav, scratch, me_xy, goal, obs.t);
                return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), goal);
            }
        }

        if (want_cubes || slot1_empty)
            && me.hp > 0.45 * me.max_hp
            && obs.t - world.last_damage_t > 2.5
            && !protecting_regen
        {
            if let Some(cr) = obs
                .crates
                .iter()
                .filter(|c| {
                    is_ground_level(map, c.pos)
                        && dist2d(me.pos[0], me.pos[1], c.pos[0], c.pos[1]) <= 115.0
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
                    let wp = self.follow_astar(nav, scratch, me_xy, goal, obs.t);
                    return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), goal);
                }
            }
        }

        if dist_next > safe_r * 0.65 {
            let wp = self.follow_astar(nav, scratch, me_xy, nc, obs.t);
            return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), nc);
        }

        let clamp_next_safe = |mut px: f64, mut py: f64| -> [f64; 2] {
            let hunt_safe_r = (obs.zone.next_radius - 18.0).max(0.0);
            let dh_nc = dist2d(px, py, nc[0], nc[1]);
            if dh_nc > hunt_safe_r {
                if hunt_safe_r > 1.0 {
                    let s = hunt_safe_r / dh_nc;
                    px = nc[0] + (px - nc[0]) * s;
                    py = nc[1] + (py - nc[1]) * s;
                } else {
                    px = nc[0];
                    py = nc[1];
                }
            }
            [px, py]
        };

        // Near sound hunt (< 60 m): immediate push while healthy
        if world.is_huntable_sound(obs.t) && world.last_sound_is_near && me.hp >= 0.60 * me.max_hp
        {
            if let Some(b) = world.last_sound_bearing {
                let goal = clamp_next_safe(me.pos[0] + b.cos() * 75.0, me.pos[1] + b.sin() * 75.0);
                self.tactic = CombatTactic::AggressivePush;
                let wp = self.follow_astar(nav, scratch, me_xy, goal, obs.t);
                return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), goal);
            }
        }

        // Early-game 50 m unvisited-grid cube snowball until stat_cap cubes (or t >= 200 s)
        if me.cubes < stat_cap && obs.t < 200.0 {
            let explore_invalid = self.explore_goal.is_none_or(|g| {
                obs.zone.next_radius > 24.0
                    && dist2d(g[0], g[1], nc[0], nc[1]) > (obs.zone.next_radius - 16.0).max(0.0)
            });
            let arrived_explore = self
                .explore_goal
                .is_some_and(|g| dist2d(me.pos[0], me.pos[1], g[0], g[1]) <= 22.0);
            let timed_explore = obs.t - self.explore_goal_t >= 10.0;

            if explore_invalid || arrived_explore || timed_explore {
                let safe_ex_r = (obs.zone.next_radius - 20.0).max(0.0);
                let base_ang = ((obs.tick % 64) as f64) * 0.37;
                let mut best_ex: Option<(f64, [f64; 2])> = None;

                for k in 0..16 {
                    let theta = base_ang + (k as f64) * (2.0 * PI / 16.0);
                    let rad = if k % 2 == 0 { 85.0 } else { 150.0 };
                    let px = me.pos[0] + theta.cos() * rad;
                    let py = me.pos[1] + theta.sin() * rad;
                    if obs.zone.next_radius > 24.0 && dist2d(px, py, nc[0], nc[1]) > safe_ex_r {
                        continue;
                    }
                    let (cx, cy) = nav.world_to_cell(px, py);
                    if !nav.walkable[cy * nav.nx + cx] {
                        continue;
                    }
                    let cell = ((px / 50.0).floor() as i32, (py / 50.0).floor() as i32);
                    let mut score = if !world.visited_cells.contains(&cell) {
                        120.0
                    } else {
                        0.0
                    };
                    for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                        if !world.visited_cells.contains(&(cell.0 + dx, cell.1 + dy)) {
                            score += 25.0;
                        }
                    }
                    score -= 0.12 * rad;
                    if best_ex.is_none_or(|(bs, _)| score > bs) {
                        best_ex = Some((score, [px, py]));
                    }
                }

                self.explore_goal = best_ex.map(|(_, pt)| pt);
                self.explore_goal_t = obs.t;
            }

            if let Some(eg) = self.explore_goal {
                let wp = self.follow_astar(nav, scratch, me_xy, eg, obs.t);
                return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), eg);
            }
        }

        // 15 s hunt memory after enemy leaves 4.5 s tracking
        if me.hp >= 0.62 * me.max_hp {
            if let Some((hpos, _)) = world.last_hunt_pos {
                let goal = clamp_next_safe(hpos[0], hpos[1]);
                self.tactic = CombatTactic::AggressivePush;
                let wp = self.follow_astar(nav, scratch, me_xy, goal, obs.t);
                return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), goal);
            }
        }

        // Mid/far sound hunting when healthy
        if world.is_huntable_sound(obs.t) && me.hp >= 0.62 * me.max_hp {
            if let Some(b) = world.last_sound_bearing {
                let hunt_d = match world.last_sound_dist.as_str() {
                    "near" => 75.0,
                    "mid" => 125.0,
                    _ => 145.0,
                };
                let goal = clamp_next_safe(me.pos[0] + b.cos() * hunt_d, me.pos[1] + b.sin() * hunt_d);
                self.tactic = CombatTactic::AggressivePush;
                let wp = self.follow_astar(nav, scratch, me_xy, goal, obs.t);
                return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), goal);
            }
        }

        // Active sweep patrol across corners inside next_zone (never camp stationary in EarlyFarm)
        let anchor_invalid = self.ambush_anchor.is_none_or(|a| {
            obs.zone.next_radius > 24.0
                && dist2d(a[0], a[1], nc[0], nc[1]) > (obs.zone.next_radius - 14.0).max(0.0)
        });
        let arrived_anchor = self
            .ambush_anchor
            .is_some_and(|a| dist2d(me.pos[0], me.pos[1], a[0], a[1]) <= 18.0);
        let timed_refresh = obs.t - self.ambush_anchor_t >= 10.0;

        if anchor_invalid || arrived_anchor || timed_refresh {
            self.prev_ambush_anchor = self.ambush_anchor;
            self.ambush_anchor = nav.find_cover_anchor(
                me.pos,
                obs.zone.center,
                obs.zone.radius,
                nc,
                obs.zone.next_radius,
                self.prev_ambush_anchor,
                180.0,
            );
            self.ambush_anchor_t = obs.t;
        }

        if let Some(anchor) = self.ambush_anchor {
            if dist2d(me.pos[0], me.pos[1], anchor[0], anchor[1]) > 6.0 {
                let wp = self.follow_astar(nav, scratch, me_xy, anchor, obs.t);
                return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), anchor);
            }
        }

        let ang = (obs.t * 0.25) + (world.strafe_sign * 0.5);
        let patrol_r = (safe_r * 0.45).min(110.0);
        let patrol_pt = [nc[0] + patrol_r * ang.cos(), nc[1] + patrol_r * ang.sin()];
        let wp = self.follow_astar(nav, scratch, me_xy, patrol_pt, obs.t);
        (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), patrol_pt)
    }

    pub fn follow_astar(
        &mut self,
        nav: &NavGrid,
        scratch: &mut NavScratch,
        start: [f64; 2],
        goal: [f64; 2],
        t: f64,
    ) -> [f64; 2] {
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
            self.cached_path = nav.find_path(start, goal, 2400, scratch);
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

    pub fn find_best_supply_drop<'a>(
        &self,
        world: &WorldState,
        obs: &'a Observation,
        los_mechs: &[&TargetCandidate],
    ) -> Option<&'a DropObs> {
        let me = &obs.self_obs;
        let max_pull = if me.hp < 0.70 * me.max_hp {
            340.0
        } else {
            220.0
        };
        let in_close_duel = los_mechs.iter().any(|m| m.dist < 60.0);
        let mut best: Option<(f64, &'a DropObs)> = None;
        for d in &obs.drops {
            if dist2d(d.pos[0], d.pos[1], obs.zone.center[0], obs.zone.center[1])
                > obs.zone.radius - 10.0
            {
                continue;
            }
            let campers = world
                .tracked_enemies
                .values()
                .filter(|e| dist2d(e.pos[0], e.pos[1], d.pos[0], d.pos[1]) <= 25.0)
                .count();
            if campers >= 2 {
                continue;
            }
            let dist = dist2d(me.pos[0], me.pos[1], d.pos[0], d.pos[1]);
            let eta = dist / 14.0;
            let arrival_ok = d.lands_in <= (eta + 4.5).max(8.0);
            let duel_ok = !in_close_duel || (d.lands_in <= 3.0 && dist <= 28.0);
            if arrival_ok
                && duel_ok
                && dist <= max_pull
                && (los_mechs.len() <= 1 || dist <= d.radius + 15.0)
                && best.is_none_or(|(bd, _)| dist < bd)
            {
                best = Some((dist, d));
            }
        }
        best.map(|(_, d)| d)
    }

    pub fn find_best_weapon_goal(
        &self,
        map: &Map,
        world: &WorldState,
        obs: &Observation,
        los_mechs: &[&TargetCandidate],
    ) -> Option<[f64; 2]> {
        let me = &obs.self_obs;
        let slot1_empty = me.weapons.get(1).and_then(|w| w.as_ref()).is_none();
        let have_lightning = me
            .weapons
            .iter()
            .flatten()
            .any(|w| w.kind == WeaponKind::Lightning);
        let have_laser = me
            .weapons
            .iter()
            .flatten()
            .any(|w| w.kind == WeaponKind::Laser);

        let close_los_enemy = los_mechs.iter().any(|m| m.dist < 60.0);
        let any_los_enemy = !los_mechs.is_empty();
        let max_seek_d = if slot1_empty {
            210.0
        } else if !have_lightning || (!have_laser && obs.alive <= 12) {
            115.0
        } else {
            65.0
        };
        let mut best: Option<(f64, u64, [f64; 2])> = None;

        for (&id, &(kind, pos, _)) in &world.last_pickup_seen {
            if kind == WeaponKind::Mortar || world.blacklisted_pickups.contains_key(&id) {
                continue;
            }
            if me.weapons.iter().flatten().any(|w| w.kind == kind) {
                continue;
            }
            if !is_ground_level(map, pos) {
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
            } else if any_los_enemy || swap_slot_for(me, kind, obs.alive).is_none() {
                continue;
            }

            let score = prio as f64 * 45.0 - d;
            if best.is_none_or(|(bs, bid, _)| score > bs || (score == bs && id < bid)) {
                best = Some((score, id, [pos[0], pos[1]]));
            }
        }

        best.map(|(_, _, pt)| pt)
    }
}

/// Preferred loadout [Laser, Lightning] for corner peek-trades and anti-laser shorting.
fn swap_slot_for(me: &SelfObs, new_kind: WeaponKind, alive: u64) -> Option<usize> {
    if new_kind == WeaponKind::Mortar {
        return None;
    }
    if let Some(m_slot) = me
        .weapons
        .iter()
        .take(2)
        .position(|w| w.as_ref().is_some_and(|w| w.kind == WeaponKind::Mortar))
    {
        return Some(m_slot);
    }
    let has_gun_upgrades = me.upgrades.iter().any(|u| u.starts_with("gun_"));
    let has_laser_upgrades = me.upgrades.iter().any(|u| u.starts_with("laser_"));
    let laser_slot = me
        .weapons
        .iter()
        .take(2)
        .position(|w| w.as_ref().is_some_and(|w| w.kind == WeaponKind::Laser));
    let gun_slot = me
        .weapons
        .iter()
        .take(2)
        .position(|w| w.as_ref().is_some_and(|w| w.kind == WeaponKind::Gun));

    match new_kind {
        WeaponKind::Lightning => {
            if let Some(gs) = gun_slot {
                if !has_gun_upgrades || has_laser_upgrades {
                    return Some(gs);
                }
            }
            if let Some(ls) = laser_slot {
                if !has_laser_upgrades {
                    return Some(ls);
                }
            }
            gun_slot.or(laser_slot)
        }
        WeaponKind::Laser => {
            if let Some(gs) = gun_slot {
                if !has_gun_upgrades && alive <= 12 {
                    return Some(gs);
                }
            }
            None
        }
        _ => None,
    }
}

#[inline]
fn zigzag_cover_dir(
    me_pos: [f64; 3],
    wp: [f64; 2],
    cp: [f64; 2],
    los_mech_count: usize,
    strafe_sign: f64,
) -> [f64; 2] {
    let base = unit2d(wp[0] - me_pos[0], wp[1] - me_pos[1]);
    if los_mech_count == 0 || dist2d(me_pos[0], me_pos[1], cp[0], cp[1]) <= 8.0 {
        return base;
    }
    let ang = 0.52 * strafe_sign;
    let (s, c) = ang.sin_cos();
    [base[0] * c - base[1] * s, base[0] * s + base[1] * c]
}

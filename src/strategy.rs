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
    pub combat_cover_target: Option<[f64; 2]>,
    pub combat_cover_t: f64,
    pub ambush_anchor: Option<[f64; 2]>,
    pub ambush_anchor_t: f64,
    pub prev_ambush_anchor: Option<[f64; 2]>,
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
            combat_cover_target: None,
            combat_cover_t: -100.0,
            ambush_anchor: None,
            ambush_anchor_t: -100.0,
            prev_ambush_anchor: None,
        }
    }

    pub fn pick_upgrade(&self, config: &Config, offer: &Offer, me: &SelfObs) -> usize {
        let mut best_idx = 0usize;
        let mut best_score = f64::NEG_INFINITY;

        for (i, choice) in offer.choices.iter().enumerate() {
            let score = if let Some(def) = config.upgrades.iter().find(|u| u.id == choice.id) {
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
        let threats: Vec<[f64; 3]> = world
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
        if want_regen
            && (self
                .ensure_regen_cover(map, nav, world, obs, eye_h, los_count)
                .is_some()
                || (self.posture == Posture::EmergencyRegen && los_count == 0))
        {
            self.posture = Posture::EmergencyRegen;
            return;
        }

        if self.find_best_supply_drop(world, obs, los_count).is_some() {
            self.posture = Posture::SupplyDrop;
            return;
        }

        if self.find_best_weapon_goal(map, world, obs, los_mechs).is_some() {
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
        world: &WorldState,
        obs: &Observation,
        eye_h: f64,
        mech_cands: &[TargetCandidate],
        los_mechs: &[&TargetCandidate],
        combat_enemy: Option<&TargetCandidate>,
        farm_target: Option<&TargetCandidate>,
        immediate_kill_avail: bool,
    ) -> ([f64; 2], [f64; 2]) {
        self.tactic = CombatTactic::None;
        self.peek_target = None;

        let me = &obs.self_obs;
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
                    return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), cp);
                }
                if let Some((_, th)) = world.closest_tracked_enemy_within(me.pos, 220.0) {
                    let away = unit2d(me.pos[0] - th.pos[0], me.pos[1] - th.pos[1]);
                    let zdir = unit2d(nc[0] - me.pos[0], nc[1] - me.pos[1]);
                    let goal = [
                        me.pos[0] + (away[0] * 0.75 + zdir[0] * 0.35) * 40.0,
                        me.pos[1] + (away[1] * 0.75 + zdir[1] * 0.35) * 40.0,
                    ];
                    let wp = self.follow_astar(nav, scratch, me_xy, goal, obs.t);
                    return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), goal);
                }
                return (unit2d(nc[0] - me.pos[0], nc[1] - me.pos[1]), nc);
            }
            Posture::SupplyDrop => {
                if let Some(drop) = self.find_best_supply_drop(world, obs, los_mech_count) {
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
                        && los_mechs.iter().any(|m| m.charging && m.dist <= 45.0)
                });
                let enemy_laser_duck = !immediate_kill_avail
                    && !lt_ready_for_charger
                    && our_laser_charge_frac < 0.65
                    && los_mechs.iter().any(|e| e.charging);

                let our_gun_low = me.weapons.iter().flatten().any(|w| {
                    w.kind == WeaponKind::Gun
                        && (w.reload.unwrap_or(0.0) > 0.0 || w.ammo.unwrap_or(12.0) <= 5.0)
                });
                let our_laser_on_cd = me.weapons.iter().flatten().any(|w| {
                    w.kind == WeaponKind::Laser && w.charge.unwrap_or(0.0) <= 0.0 && w.cooldown > 0.35
                });
                let near_cover_14 = nav.has_near_cover_within(me.pos, 14.0);
                let wait_enemy_reload_duck = !immediate_kill_avail
                    && combat_enemy.is_some_and(|e| {
                        !e.is_vulnerable && (me.hp < 0.72 * me.max_hp || e.hp > me.hp + 200.0)
                    })
                    && (los_mech_count == 0 || near_cover_14)
                    && (our_gun_low || our_laser_on_cd);

                // 1. CoverDuck: outnumbered (2+ LOS), dry/reloading, enemy charging laser, or waiting behind cover for enemy reload
                if los_mech_count >= 2
                    || no_weapon_can_shoot_now
                    || enemy_laser_duck
                    || wait_enemy_reload_duck
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
                        threats = world
                            .tracked_enemies
                            .values()
                            .filter(|e| dist2d(me.pos[0], me.pos[1], e.pos[0], e.pos[1]) <= 220.0)
                            .map(|e| [e.pos[0], e.pos[1], e.pos[2] + 7.5])
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
                                48.0,
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
                            return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), cp);
                        }
                    }
                }

                if let Some(e) = combat_enemy {
                    // 2. Enemy behind wall (!e.has_los): slice-the-pie corner peek or hold cover ambush
                    if !e.has_los {
                        let other_threats: Vec<[f64; 3]> = world
                            .tracked_enemies
                            .iter()
                            .filter(|(&id, te)| {
                                id != e.id
                                    && dist2d(me.pos[0], me.pos[1], te.pos[0], te.pos[1]) <= 200.0
                            })
                            .map(|(_, te)| [te.pos[0], te.pos[1], te.pos[2] + 7.5])
                            .collect();

                        if e.is_vulnerable || (e.dist <= 135.0 && !e.charging) {
                            let target_eye = [e.pos[0], e.pos[1], e.pos[2] + 7.5];
                            if let Some(pp) = nav.find_peek_point(
                                map,
                                me.pos,
                                eye_h,
                                target_eye,
                                &other_threats,
                                obs.zone.center,
                                obs.zone.radius,
                                18.0,
                            ) {
                                self.tactic = CombatTactic::CornerPeek;
                                self.peek_target = Some(pp);
                                let wp = self.follow_astar(nav, scratch, me_xy, pp, obs.t);
                                return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), pp);
                            } else if other_threats.is_empty()
                                && e.dist > 35.0
                                && me.hp > 0.55 * me.max_hp
                            {
                                self.tactic = CombatTactic::AggressivePush;
                                let goal = [e.pos[0], e.pos[1]];
                                let wp = self.follow_astar(nav, scratch, me_xy, goal, obs.t);
                                return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), goal);
                            }
                        }

                        self.tactic = CombatTactic::CoverAmbush;
                        if dist_next > safe_r * 0.85 {
                            let wp = self.follow_astar(nav, scratch, me_xy, nc, obs.t);
                            return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), nc);
                        }
                        return ([0.0, 0.0], me_xy);
                    }

                    // 3. Direct 1v1 engagement: AggressivePush when enemy is vulnerable, else cover-edged OpenStrafe
                    let to_e = unit2d(e.pos[0] - me.pos[0], e.pos[1] - me.pos[1]);
                    let perp = [-to_e[1] * world.strafe_sign, to_e[0] * world.strafe_sign];
                    let has_lightning = me
                        .weapons
                        .iter()
                        .flatten()
                        .any(|w| w.kind == WeaponKind::Lightning);
                    let enemy_has_laser = e.weapons.contains(&WeaponKind::Laser);
                    let base_pref_dist: f64 =
                        if has_lightning && me.energy > 30.0 && (e.charging || e.dist < 52.0) {
                            30.0
                        } else if enemy_has_laser && !have_laser {
                            if has_lightning { 30.0 } else { 45.0 }
                        } else if me.hp < 0.50 * me.max_hp {
                            92.0
                        } else {
                            64.0
                        };

                    let (pref_dist, push_w) = if e.is_vulnerable {
                        self.tactic = CombatTactic::AggressivePush;
                        ((base_pref_dist - 15.0).max(20.0), 0.70)
                    } else {
                        self.tactic = CombatTactic::OpenStrafe;
                        (base_pref_dist, 0.55)
                    };

                    let d2 = dist2d(me.pos[0], me.pos[1], e.pos[0], e.pos[1]);
                    let radial_w = if d2 < pref_dist - 12.0 {
                        -0.65
                    } else if d2 > pref_dist + if e.is_vulnerable { 8.0 } else { 18.0 } {
                        push_w
                    } else {
                        0.0
                    };
                    let mut gx = to_e[0] * radial_w + perp[0] * 0.85;
                    let mut gy = to_e[1] * radial_w + perp[1] * 0.85;

                    if !e.is_vulnerable {
                        if let Some(corner) = nav.find_cover_anchor(
                            me.pos,
                            obs.zone.center,
                            obs.zone.radius,
                            nc,
                            obs.zone.next_radius,
                            None,
                            26.0,
                        ) {
                            if dist2d(me.pos[0], me.pos[1], corner[0], corner[1]) > 12.0 {
                                let cdir = unit2d(corner[0] - me.pos[0], corner[1] - me.pos[1]);
                                gx += cdir[0] * 0.25;
                                gy += cdir[1] * 0.25;
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

        // EarlyFarm / fallback: ground cubes -> NPCs -> ground crates -> safe interior -> CoverAmbush anchor
        if let Some(cube) = obs
            .cubes
            .iter()
            .filter(|c| {
                is_ground_level(map, c.pos)
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
            let wp = self.follow_astar(nav, scratch, me_xy, goal, obs.t);
            return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), goal);
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

        if me.hp > 0.45 * me.max_hp && obs.t - world.last_damage_t > 2.5 {
            if let Some(cr) = obs
                .crates
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

        // Cover-Anchored 360° scan: hold/rotate between building corners inside next_zone instead of exposed fields
        let slot1_empty = me.weapons.get(1).and_then(|w| w.as_ref()).is_none();
        let anchor_invalid = self.ambush_anchor.is_none_or(|a| {
            obs.zone.next_radius > 24.0
                && dist2d(a[0], a[1], nc[0], nc[1]) > (obs.zone.next_radius - 14.0).max(0.0)
        });
        let explore_refresh = slot1_empty && obs.t < 45.0 && (obs.t - self.ambush_anchor_t >= 8.0);
        let periodic_refresh = obs.t - self.ambush_anchor_t >= 20.0;

        if anchor_invalid || explore_refresh || periodic_refresh {
            self.prev_ambush_anchor = self.ambush_anchor;
            self.ambush_anchor = nav.find_cover_anchor(
                me.pos,
                obs.zone.center,
                obs.zone.radius,
                nc,
                obs.zone.next_radius,
                if explore_refresh {
                    self.prev_ambush_anchor
                } else {
                    None
                },
                180.0,
            );
            self.ambush_anchor_t = obs.t;
        }

        if let Some(anchor) = self.ambush_anchor {
            self.tactic = CombatTactic::CoverAmbush;
            if dist2d(me.pos[0], me.pos[1], anchor[0], anchor[1]) <= 4.5 {
                return ([0.0, 0.0], anchor);
            }
            let wp = self.follow_astar(nav, scratch, me_xy, anchor, obs.t);
            return (unit2d(wp[0] - me.pos[0], wp[1] - me.pos[1]), anchor);
        }

        let ang = (obs.t * 0.25) + (world.strafe_sign * 0.5);
        let patrol_r = (safe_r * 0.40).min(110.0);
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
            let campers = world
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

    pub fn find_best_weapon_goal(
        &self,
        map: &Map,
        world: &WorldState,
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

        for (id, &(kind, pos, _)) in &world.last_pickup_seen {
            if kind == WeaponKind::Mortar || world.blacklisted_pickups.contains_key(id) {
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
}

use crate::geom::{angle_diff, direct_aim, dist3d};
use crate::map::Map;
use crate::protocol::{stat, Config, Observation, SelfObs, WeaponKind, WeaponObs};
use crate::world::WorldState;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TargetKind {
    Mech,
    Npc,
    Crate,
}

#[derive(Clone, Debug)]
pub(crate) struct TargetCandidate {
    pub id: u64,
    pub kind: TargetKind,
    pub pos: [f64; 3],
    pub vel: [f64; 3],
    pub est_accel_z: f64,
    pub aim: [f64; 2],
    #[allow(dead_code)]
    pub com_z: f64,
    pub aim_z: f64,
    pub aim_half_h: f64,
    pub radius: f64,
    pub hp: f64,
    pub max_hp: f64,
    pub weapons: Vec<WeaponKind>,
    pub charging: bool,
    pub has_los: bool,
    pub dist: f64,
    pub is_reloading: bool,
    pub is_laser_on_cd: bool,
    pub is_vulnerable: bool,
    pub is_reversing: bool,
}

pub(crate) fn select_targets(
    obs: &Observation,
    map: &Map,
    config: &Config,
    world: &WorldState,
    muzzle: [f64; 3],
) -> (Vec<TargetCandidate>, Option<TargetCandidate>) {
    let me = &obs.self_obs;
    let base_mech_h = stat(&config.mech, "height", 9.0);
    let base_mech_r = stat(&config.mech, "radius", 2.5);
    let size_per_cube = config.cubes.size_per_cube;
    let npc_r = stat(&config.npc, "radius", 1.8);
    let npc_h = stat(&config.npc, "height", 3.5);
    let crate_r = stat(&config.crates, "radius", 1.4);
    let crate_h = stat(&config.crates, "height", 2.4);
    let regen_delay = stat(&me.stats, "regen_delay", 5.0);
    let protecting_regen = me.hp < 0.72 * me.max_hp
        && (obs.t - world.last_combat_t) >= 2.0
        && (obs.t - world.last_combat_t) <= regen_delay + 12.0;

    let mut mech_cands = Vec::new();
    let mut best_farm: Option<(f64, TargetCandidate)> = None;

    for actor in &obs.mechs {
        let d = dist3d(muzzle, actor.pos);
        if actor.kind == "mech" {
            let scale = 1.0 + size_per_cube * (actor.cubes.unwrap_or(0) as f64);
            let mech_h = base_mech_h * scale;
            let com_z = actor.pos[2] + 0.50 * mech_h;
            let upper_z = actor.pos[2] + 0.82 * mech_h;
            let lower_z = actor.pos[2] + 0.22 * mech_h;
            let (has_los, aim_z) = if map.has_los(muzzle, [actor.pos[0], actor.pos[1], com_z]) {
                (true, com_z)
            } else if map.has_los(muzzle, [actor.pos[0], actor.pos[1], upper_z]) {
                (true, upper_z)
            } else if map.has_los(muzzle, [actor.pos[0], actor.pos[1], lower_z]) {
                (true, lower_z)
            } else {
                (false, com_z)
            };
            let aim_off = aim_z - actor.pos[2];
            let aim_half_h = (mech_h - aim_off).min(aim_off).max(1.0);
            let radius = base_mech_r * scale;
            let tracked = world.tracked_enemies.get(&actor.id);
            let est_az = tracked.map(|e| e.est_accel_z).unwrap_or(0.0);
            let is_reloading = tracked.is_some_and(|e| e.is_reloading(obs.t));
            let is_laser_on_cd = tracked.is_some_and(|e| e.is_laser_on_cd(obs.t));
            let is_vulnerable = tracked.is_some_and(|e| e.is_vulnerable(obs.t));
            let is_reversing = tracked.is_some_and(|e| e.is_reversing(obs.t));

            mech_cands.push(TargetCandidate {
                id: actor.id,
                kind: TargetKind::Mech,
                pos: actor.pos,
                vel: actor.vel,
                est_accel_z: est_az,
                aim: actor.aim,
                com_z,
                aim_z,
                aim_half_h,
                radius,
                hp: actor.hp.unwrap_or(1400.0),
                max_hp: actor.max_hp.unwrap_or(1400.0),
                weapons: actor.weapons.clone(),
                charging: actor.charging,
                has_los,
                dist: d,
                is_reloading,
                is_laser_on_cd,
                is_vulnerable,
                is_reversing,
            });
        } else if actor.kind == "npc"
            && d <= 125.0
            && me.hp > 0.45 * me.max_hp
            && obs.t - world.last_damage_t > 2.5
            && !protecting_regen
        {
            let com_z = actor.pos[2] + 0.5 * npc_h;
            if map.has_los(muzzle, [actor.pos[0], actor.pos[1], com_z]) {
                let cand = TargetCandidate {
                    id: actor.id,
                    kind: TargetKind::Npc,
                    pos: actor.pos,
                    vel: actor.vel,
                    est_accel_z: 0.0,
                    aim: actor.aim,
                    com_z,
                    aim_z: com_z,
                    aim_half_h: (com_z - actor.pos[2]).max(1.0),
                    radius: npc_r,
                    hp: actor.hp.unwrap_or(350.0),
                    max_hp: actor.max_hp.unwrap_or(350.0),
                    weapons: Vec::new(),
                    charging: false,
                    has_los: true,
                    dist: d,
                    is_reloading: false,
                    is_laser_on_cd: false,
                    is_vulnerable: false,
                    is_reversing: false,
                };
                let score = 200.0 - d;
                if best_farm.as_ref().is_none_or(|(bs, _)| score > *bs) {
                    best_farm = Some((score, cand));
                }
            }
        }
    }

    if best_farm.is_none()
        && me.hp > 0.45 * me.max_hp
        && obs.t - world.last_damage_t > 2.5
        && !protecting_regen
    {
        for c in &obs.crates {
            let d = dist3d(muzzle, c.pos);
            if d > 110.0 {
                continue;
            }
            let com_z = c.pos[2] + 0.5 * crate_h;
            if map.has_los(muzzle, [c.pos[0], c.pos[1], com_z]) {
                let cand = TargetCandidate {
                    id: c.id,
                    kind: TargetKind::Crate,
                    pos: c.pos,
                    vel: [0.0, 0.0, 0.0],
                    est_accel_z: 0.0,
                    aim: [0.0, 0.0],
                    com_z,
                    aim_z: com_z,
                    aim_half_h: (com_z - c.pos[2]).max(1.0),
                    radius: crate_r,
                    hp: c.hp.unwrap_or(300.0),
                    max_hp: 300.0,
                    weapons: Vec::new(),
                    charging: false,
                    has_los: true,
                    dist: d,
                    is_reloading: false,
                    is_laser_on_cd: false,
                    is_vulnerable: false,
                    is_reversing: false,
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

pub(crate) fn pick_best_mech_target(
    cands: &[TargetCandidate],
    me: &SelfObs,
    last_damage_by: Option<u64>,
    last_target_id: Option<u64>,
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
        score += (c.max_hp - c.hp).max(0.0) * 0.35;
        if c.hp <= 320.0 {
            score += 200.0;
        } else if c.hp <= 520.0 {
            score += 120.0;
        }
        if last_target_id == Some(c.id) && c.hp < 0.90 * c.max_hp {
            score += 35.0;
        }
        if c.is_reloading {
            score += 55.0;
        }
        if c.is_laser_on_cd {
            score += 35.0;
        }
        if have_laser && c.weapons.contains(&WeaponKind::Mortar) {
            score += 95.0; // Cook-off!
        }
        if have_lightning {
            if c.dist <= 50.0 {
                if c.charging {
                    score += 130.0;
                } else if c.weapons.contains(&WeaponKind::Laser) {
                    score += 65.0;
                }
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
        if last_damage_by == Some(c.id) {
            score += 40.0;
        }

        if best_los.is_none_or(|(bs, _)| score > bs) {
            best_los = Some((score, c));
        }
    }

    best_los.map(|(_, c)| c.clone())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn eval_combat(
    map: &Map,
    config: &Config,
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
    let mut chosen_aim: Option<[f64; 2]>;
    let debug_pt = Some([tgt.pos[0], tgt.pos[1], tgt.aim_z]);

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

    let other_weapon_ready = laser_slot.is_some_and(|ls| {
        let lw = me.weapons[ls].as_ref().unwrap();
        let lr = stat(&lw.stats, "range", 280.0);
        tgt.has_los && tgt.dist <= lr - 2.0 && (lw.charge.unwrap_or(0.0) > 0.0 || lw.cooldown <= 0.15)
    }) || lightning_slot.is_some_and(|lts| {
        let lt = me.weapons[lts].as_ref().unwrap();
        let ltr = stat(&lt.stats, "range", 45.0);
        tgt.has_los && tgt.dist <= ltr - 0.5 && me.energy >= 12.0
    });

    let eval_gun_slot = |slot: usize| -> ([f64; 2], bool, bool, bool) {
        let w = me.weapons[slot].as_ref().unwrap();
        eval_gun(
            map,
            config,
            me.aim,
            muzzle,
            mech_radius,
            w,
            tgt,
            min_pitch,
            max_pitch,
            other_weapon_ready,
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
        eval_lightning(me.aim, me.energy, muzzle, w, tgt, aim_yaw, cluster_count)
    };

    // 1. Active charging laser takes priority for hold/release
    if let Some(ls) = active_laser {
        let w = me.weapons[ls].as_ref().unwrap();
        let full_charge = stat(&w.stats, "charge", 0.75);
        let charge = w.charge.unwrap_or(0.0);
        let (l_aim, should_hold) =
            eval_laser(map, config, me.aim, muzzle, me.cubes, w, tgt, min_pitch, max_pitch);
        fire[ls] = should_hold;

        if should_hold && charge < full_charge - 0.20 {
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

    // 2. Lightning close-range check (paired simultaneously with Gun or Laser)
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

            let direct = direct_aim(muzzle, [tgt.pos[0], tgt.pos[1], tgt.aim_z]);
            let (lt_aim, lt_fire) = eval_lt_slot(lts, direct[0]);
            fire[lts] = lt_fire;
            chosen_aim = Some(lt_aim);

            // Simultaneous Lightning + Laser charge at <= 45 m when Laser is off cooldown
            if let Some(ls) = laser_slot {
                let lw = me.weapons[ls].as_ref().unwrap();
                let l_range = stat(&lw.stats, "range", 280.0);
                let allow_laser_farm = matches!(tgt.kind, TargetKind::Npc | TargetKind::Crate)
                    && me.hp > 0.55 * me.max_hp;
                if tgt.dist <= l_range - 2.0
                    && lw.cooldown <= 0.08
                    && (matches!(tgt.kind, TargetKind::Mech) || allow_laser_farm)
                    && me.hp > 0.35 * me.max_hp
                    && !tgt.charging
                {
                    let (l_aim, should_hold) = eval_laser(
                        map,
                        config,
                        me.aim,
                        muzzle,
                        me.cubes,
                        lw,
                        tgt,
                        min_pitch,
                        max_pitch,
                    );
                    fire[ls] = should_hold;
                    chosen_aim = Some(l_aim);
                }
            }

            return (fire, reload, chosen_aim, debug_pt);
        }
    }

    // 3. Laser ready check
    if let Some(ls) = laser_slot {
        let w = me.weapons[ls].as_ref().unwrap();
        let l_range = stat(&w.stats, "range", 280.0);
        let allow_laser_farm =
            matches!(tgt.kind, TargetKind::Npc | TargetKind::Crate) && me.hp > 0.55 * me.max_hp;
        if tgt.has_los
            && tgt.dist <= l_range - 2.0
            && w.cooldown <= 0.08
            && (matches!(tgt.kind, TargetKind::Mech) || allow_laser_farm)
        {
            let (l_aim, should_hold) =
                eval_laser(map, config, me.aim, muzzle, me.cubes, w, tgt, min_pitch, max_pitch);
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

    chosen_aim = Some(direct_aim(muzzle, [tgt.pos[0], tgt.pos[1], tgt.aim_z]));
    (fire, reload, chosen_aim, debug_pt)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn eval_precharge_laser(
    obs: &Observation,
    config: &Config,
    world: &WorldState,
    muzzle: [f64; 3],
    min_pitch: f64,
    max_pitch: f64,
    peek_target: Option<[f64; 2]>,
    los_mech_count: usize,
) -> ([bool; 2], Option<[f64; 2]>) {
    let me = &obs.self_obs;
    let mut fire = [false, false];
    let mut pre_aim: Option<[f64; 2]> = None;

    let mut active_laser_idx: Option<usize> = None;
    let mut ready_laser_idx: Option<usize> = None;

    for (idx, wopt) in me.weapons.iter().enumerate().take(2) {
        if let Some(w) = wopt {
            if w.kind == WeaponKind::Laser {
                let charge = w.charge.unwrap_or(0.0);
                let full_charge = stat(&w.stats, "charge", 0.75);
                if charge > 0.0 {
                    fire[idx] = charge < full_charge;
                    active_laser_idx = Some(idx);
                } else if w.cooldown <= 0.08 {
                    ready_laser_idx = Some(idx);
                }
            }
        }
    }

    let near_peek = peek_target.is_some_and(|pp| {
        let d_pp = (pp[0] - me.pos[0]).hypot(pp[1] - me.pos[1]);
        d_pp <= 4.5
    });

    if los_mech_count == 0 && near_peek {
        if let Some((_, enemy)) = world.closest_tracked_enemy_within(me.pos, 180.0) {
            let base_mech_h = stat(&config.mech, "height", 9.0);
            let aim_pt = [enemy.pos[0], enemy.pos[1], enemy.pos[2] + 0.5 * base_mech_h];
            let mut aim = direct_aim(muzzle, aim_pt);
            aim[1] = aim[1].clamp(min_pitch, max_pitch);

            if active_laser_idx.is_some() {
                pre_aim = Some(aim);
            } else if me.hp > 0.45 * me.max_hp {
                if let Some(idx) = ready_laser_idx {
                    fire[idx] = true;
                    pre_aim = Some(aim);
                }
            }
        }
    }

    (fire, pre_aim)
}

pub(crate) fn should_cover_reload(
    me: &SelfObs,
    near_mech_100: usize,
    los_mech_count: usize,
    rushing_kill_peek: bool,
    gun_fired_this_tick: bool,
    closest_los_dist: f64,
) -> bool {
    let laser_charging = me
        .weapons
        .iter()
        .flatten()
        .any(|w| w.kind == WeaponKind::Laser && w.charge.unwrap_or(0.0) > 0.0);
    if laser_charging {
        return false;
    }

    for wopt in me.weapons.iter().flatten() {
        if wopt.kind == WeaponKind::Gun && wopt.reload.unwrap_or(0.0) <= 0.0 {
            let mag = stat(&wopt.stats, "mag", 12.0);
            let ammo = wopt.ammo.unwrap_or(mag);
            if ammo < mag {
                if near_mech_100 == 0 && ammo <= 4.0 {
                    return true;
                }
                if ammo <= 4.0
                    && !gun_fired_this_tick
                    && !rushing_kill_peek
                    && (los_mech_count == 0 || closest_los_dist > 55.0)
                {
                    return true;
                }
                if los_mech_count == 0
                    && ammo <= (mag * 0.50).max(5.0)
                    && !rushing_kill_peek
                {
                    return true;
                }
            }
        }
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn eval_gun(
    map: &Map,
    config: &Config,
    cur_aim: [f64; 2],
    muzzle: [f64; 3],
    mech_radius: f64,
    w: &WeaponObs,
    tgt: &TargetCandidate,
    min_pitch: f64,
    max_pitch: f64,
    other_weapon_ready: bool,
) -> ([f64; 2], bool, bool, bool) {
    let speed = stat(&w.stats, "speed", 252.0);
    let range = stat(&w.stats, "range", 230.0);
    let spread_deg = stat(&w.stats, "spread_deg", 0.6);
    let mag = stat(&w.stats, "mag", 12.0);
    let ammo = w.ammo.unwrap_or(mag);
    let reload_left = w.reload.unwrap_or(0.0);
    let gravity = stat(&config.mech, "gravity", 22.0);

    let (aim, lead_pos) = solve_gun_lead_airborne(
        map,
        muzzle,
        tgt.pos,
        tgt.vel,
        tgt.est_accel_z,
        tgt.aim_z - tgt.pos[2],
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
    let hit_ok = gun_hit_chance_ok(
        cur_aim,
        aim,
        dist,
        speed,
        spread_deg,
        ammo,
        other_weapon_ready,
        tgt,
    );
    let los_ok = tgt.has_los && map.has_los(muzzle, lead_pos);

    (aim, ready && hit_ok && los_ok, need_reload, ready && hit_ok)
}

#[allow(clippy::too_many_arguments)]
fn gun_hit_chance_ok(
    cur_aim: [f64; 2],
    aim: [f64; 2],
    dist: f64,
    speed: f64,
    spread_deg: f64,
    ammo: f64,
    other_weapon_ready: bool,
    tgt: &TargetCandidate,
) -> bool {
    let pitch_err = (cur_aim[1] - aim[1]).abs();
    let pitch_tol = (tgt.aim_half_h * 1.35 / dist.max(4.0))
        .atan()
        .clamp(0.02, 0.35);
    if pitch_err > pitch_tol {
        return false;
    }

    let t = (dist / speed.max(1.0)).min(2.0);
    let sigma_spread = dist * spread_deg.to_radians().tan();
    let tgt_speed_xy = tgt.vel[0].hypot(tgt.vel[1]);
    let sigma_move = if matches!(tgt.kind, TargetKind::Mech) {
        if tgt.is_reversing {
            15.0 * t * t
        } else if tgt.charging || tgt_speed_xy < 1.5 {
            0.35
        } else {
            0.65
        }
    } else {
        0.30
    };
    let eff_r = if (tgt.aim_z - tgt.com_z).abs() > 0.5 {
        tgt.radius * 0.80
    } else {
        tgt.radius
    };
    let yaw_err = angle_diff(cur_aim[0], aim[0]);
    let scaled_pitch = pitch_err * (eff_r / tgt.aim_half_h.max(1.0));
    let sigma_aim = yaw_err.hypot(scaled_pitch) * dist;
    let sigma =
        (sigma_spread * sigma_spread + sigma_move * sigma_move + sigma_aim * sigma_aim).sqrt();
    let mult = if tgt.hp <= 300.0 {
        2.60
    } else if ammo <= 4.0 && !other_weapon_ready {
        1.07
    } else {
        1.48
    };
    sigma <= mult * eff_r
}

#[allow(clippy::too_many_arguments)]
fn eval_lightning(
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

    let direct = direct_aim(muzzle, [tgt.pos[0], tgt.pos[1], tgt.aim_z]);
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
pub(crate) fn eval_laser(
    map: &Map,
    config: &Config,
    cur_aim: [f64; 2],
    muzzle: [f64; 3],
    my_cubes: u64,
    w: &WeaponObs,
    tgt: &TargetCandidate,
    min_pitch: f64,
    max_pitch: f64,
) -> ([f64; 2], bool) {
    let aim_off = tgt.aim_z - tgt.pos[2];
    let hit_x = tgt.pos[0] + tgt.vel[0] * 0.05;
    let hit_y = tgt.pos[1] + tgt.vel[1] * 0.05;
    let surf_hit = map.surface_height(hit_x, hit_y, tgt.pos[2]) + aim_off;
    let hit_z = (tgt.aim_z + tgt.vel[2] * 0.05).max(surf_hit);
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

    let aligned = slew_aligned(
        cur_aim,
        aim,
        tgt.radius,
        tgt.aim_half_h,
        tgt.dist.max(5.0),
        0.95,
        0.0,
    );

    let frac = min_frac + (1.0 - min_frac) * (charge / full_charge.max(0.01)).clamp(0.0, 1.0);
    let cube_mult =
        1.0 + config.cubes.damage_per_cube * (my_cubes as f64).min(config.cubes.stat_cap);
    let cookoff_mult = if tgt.weapons.contains(&WeaponKind::Mortar) {
        1.0 + cookoff
    } else {
        1.0
    };
    let front_half = 0.5 * stat(&config.mech, "front_arc_deg", 180.0).to_radians();
    let rear_mult_cfg = stat(&config.mech, "rear_mult", 1.5);
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
        tgt.aim_z,
    ];
    let losing_los = tgt.has_los && !map.has_los(muzzle, future_pt);

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
            if tgt.charging || tgt.hp < 480.0 || cluster_count >= 2 {
                energy >= 6.0_f64.max(e_cost)
            } else {
                energy >= 12.0_f64.max(e_cost)
            }
        }
    }
}

#[inline]
pub(crate) fn weapon_desirability(kind: WeaponKind) -> u8 {
    match kind {
        WeaponKind::Lightning => 4,
        WeaponKind::Laser => 3,
        WeaponKind::Gun => 2,
        WeaponKind::Mortar => 0,
    }
}

pub(crate) fn weapon_loadout_tag(weapons: &[Option<WeaponObs>]) -> String {
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
pub(crate) fn slew_aligned(
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

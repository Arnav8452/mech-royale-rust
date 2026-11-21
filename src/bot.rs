use crate::combat::{
    self, pick_best_mech_target, select_targets, weapon_loadout_tag, TargetCandidate,
};
use crate::geom::{dist2d, wrap_angle};
use crate::map::Map;
use crate::nav::{NavGrid, NavScratch};
use crate::protocol::{stat, Action, Config, DebugDraw, DebugLine, DebugPoint, Hello, Observation};
use crate::steering;
use crate::strategy::{CombatTactic, Posture, StrategyState};
use crate::world::WorldState;

pub struct Bot {
    name: String,
    config: Config,
    map: Map,
    nav: NavGrid,
    nav_scratch: NavScratch,
    world: WorldState,
    strategy: StrategyState,
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
        let world = WorldState::new(hello.seed);
        let strategy = StrategyState::new();
        Self {
            name: "rust-bot-m6b".to_string(),
            config: hello.config,
            map,
            nav,
            nav_scratch,
            world,
            strategy,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn act(&mut self, obs: &Observation) -> Action {
        let me = &obs.self_obs;
        let t = obs.t;

        let stuck_active = self.world.update(obs, &self.map, &self.config);

        let mut action = Action {
            tick: obs.tick,
            ..Default::default()
        };

        if let Some(offer) = &obs.offer {
            action.pick = Some(self.strategy.pick_upgrade(&self.config, offer, me));
        }

        let muzzle_h = stat(&me.stats, "muzzle_height", 7.0);
        let eye_h = stat(&me.stats, "eye_height", 8.0);
        let muzzle = [me.pos[0], me.pos[1], me.pos[2] + muzzle_h];
        let mech_radius = stat(&me.stats, "radius", 2.5);
        let mech_height = stat(&me.stats, "height", 9.0);
        let min_pitch = stat(&me.stats, "min_pitch", -0.7);
        let max_pitch = stat(&me.stats, "max_pitch", 1.3);

        let (mech_cands, primary_farm) =
            select_targets(obs, &self.map, &self.config, &self.world, muzzle);
        let los_mechs: Vec<&TargetCandidate> = mech_cands.iter().filter(|c| c.has_los).collect();
        let los_mech_count = los_mechs.len();
        let near_mech_count = self
            .world
            .tracked_enemies
            .values()
            .filter(|e| dist2d(me.pos[0], me.pos[1], e.pos[0], e.pos[1]) <= 180.0)
            .count();

        let (take_id, drop_slot) =
            self.strategy
                .pick_weapon_take_and_drop(&self.config, &self.world, obs, &los_mechs);
        if let Some(pid) = take_id {
            action.take = Some(pid);
            if let Some(ds) = drop_slot {
                action.drop = Some(ds);
            }
        }

        let immediate_kill_avail = me.hp > 200.0 && los_mechs.iter().any(|m| m.hp <= 480.0);

        self.strategy.update_posture(
            &self.map,
            &self.nav,
            &self.world,
            obs,
            &los_mechs,
            near_mech_count,
            immediate_kill_avail,
            eye_h,
        );

        let primary_mech = pick_best_mech_target(
            &mech_cands,
            me,
            self.world.last_damage_by,
            self.world.last_target_id,
        );
        self.world.last_target_id = primary_mech.as_ref().map(|m| m.id);
        let any_visible_mech = primary_mech.clone().or_else(|| {
            mech_cands
                .iter()
                .min_by(|a, b| a.dist.partial_cmp(&b.dist).unwrap())
                .cloned()
        });

        let combat_target = if self.strategy.posture == Posture::EmergencyRegen {
            primary_mech.clone()
        } else {
            primary_mech.clone().or_else(|| primary_farm.clone())
        };

        // Navigation, Cover & Corner-Peek Goal (computed before combat so peek_target can drive Laser pre-charge)
        let (goal_dir, goal_pt) = self.strategy.compute_nav_goal(
            &self.map,
            &self.nav,
            &mut self.nav_scratch,
            &self.world,
            obs,
            eye_h,
            &mech_cands,
            &los_mechs,
            primary_mech.as_ref().or(any_visible_mech.as_ref()),
            primary_farm.as_ref(),
            immediate_kill_avail,
        );

        // Fire back while retreating in plain sight; hold fire once LOS broken or 1.8 s clear
        let actively_taking_fire = los_mech_count > 0
            && (t - self.world.last_damage_t < 1.8 || los_mechs.iter().any(|m| m.dist < 50.0));
        let hold_fire_for_regen = self.strategy.posture == Posture::EmergencyRegen
            && !immediate_kill_avail
            && !actively_taking_fire;

        let enemy_aiming_laser_at_me = obs.mechs.iter().any(|m| {
            if m.kind != "mech" || !m.charging {
                return false;
            }
            let ray = [m.aim[0].cos(), m.aim[0].sin()];
            let rel = [me.pos[0] - m.pos[0], me.pos[1] - m.pos[1]];
            let proj = rel[0] * ray[0] + rel[1] * ray[1];
            let perp_dist = (rel[0] * ray[1] - rel[1] * ray[0]).abs();
            proj > 0.0 && proj <= 265.0 && perp_dist <= 8.0
        });
        let avoid_laser_slow =
            enemy_aiming_laser_at_me || (los_mech_count >= 2 && me.hp < 0.55 * me.max_hp);

        let (fire, mut reload, chosen_aim, debug_aim_target) = if hold_fire_for_regen {
            ([false, false], false, None, None)
        } else if let Some(ref tgt) = combat_target {
            combat::eval_combat(
                &self.map,
                &self.config,
                obs,
                muzzle,
                mech_radius,
                min_pitch,
                max_pitch,
                &mech_cands,
                los_mech_count,
                avoid_laser_slow,
                tgt,
            )
        } else {
            let (pre_fire, pre_aim) = combat::eval_precharge_laser(
                obs,
                &self.config,
                &self.world,
                muzzle,
                min_pitch,
                max_pitch,
                self.strategy.peek_target,
                los_mech_count,
            );
            (pre_fire, false, pre_aim, None)
        };

        if fire[0] || fire[1] {
            self.world.last_combat_t = t;
        }

        // Cover-reload discipline & safe open reload
        let near_mech_100 = self
            .world
            .tracked_enemies
            .values()
            .filter(|e| dist2d(me.pos[0], me.pos[1], e.pos[0], e.pos[1]) <= 100.0)
            .count();
        let rushing_kill_peek = self.strategy.tactic == CombatTactic::CornerPeek
            && any_visible_mech
                .as_ref()
                .is_some_and(|m| m.hp <= 420.0 || m.is_vulnerable);
        if !reload && combat::should_cover_reload(me, near_mech_100, los_mech_count, rushing_kill_peek)
        {
            reload = true;
        }

        // Context Steering
        let (move_dir, dodged_hazard, aimed_laser_dodge, dodged_bullet) = steering::steer(
            &self.map,
            &self.nav,
            &self.config,
            obs,
            goal_dir,
            self.world.last_move,
            mech_radius,
            mech_height,
            self.world.strafe_sign,
        );
        self.world.last_move = move_dir;

        // Rear-armor shielding & active 360° scanning (including while hidden in EmergencyRegen)
        let final_aim = if let Some(aim) = chosen_aim {
            aim
        } else if t - self.world.last_damage_t <= 2.2 {
            if let Some(b) = self.world.last_damage_bearing {
                [b, 0.0]
            } else {
                [me.aim[0], 0.0]
            }
        } else if self.strategy.posture == Posture::EmergencyRegen
            && (los_mech_count > 0 || t - self.world.last_damage_t <= 1.5)
        {
            if let Some((_, closest_tracked)) = self.world.closest_tracked_enemy_within(me.pos, 100.0)
            {
                let yaw = (closest_tracked.pos[1] - me.pos[1])
                    .atan2(closest_tracked.pos[0] - me.pos[0]);
                [yaw, 0.0]
            } else {
                [wrap_angle(me.aim[0] + 0.95 * self.world.last_scan_dir), 0.0]
            }
        } else if t - self.world.last_sound_t <= 0.8 && self.world.last_sound_is_near {
            if let Some(b) = self.world.last_sound_bearing {
                [b, 0.0]
            } else {
                [wrap_angle(me.aim[0] + 0.95 * self.world.last_scan_dir), 0.0]
            }
        } else {
            [wrap_angle(me.aim[0] + 0.95 * self.world.last_scan_dir), 0.0]
        };

        // Boost & jump (only jump when truly wedged for 5+ ticks; never on wall-graze)
        let moving = move_dir[0].hypot(move_dir[1]) > 0.25;
        let slot1_empty = me.weapons.get(1).and_then(|w| w.as_ref()).is_none();
        let stuck_jump = me.grounded && stuck_active;
        if stuck_jump {
            if t - self.world.last_strafe_flip_t > 0.6 {
                self.world.strafe_sign = -self.world.strafe_sign;
                self.world.last_strafe_flip_t = t;
            }
            self.strategy.cached_path.clear();
            self.world.unstuck_air_boost_until = t + 0.35;
        }

        let air_unstuck_boost = !me.grounded
            && me.vel[2] > 0.0
            && t <= self.world.unstuck_air_boost_until
            && self.world.cmd_move_streak >= 8
            && me.energy > 40.0;

        let saving_for_lightning = me
            .weapons
            .iter()
            .flatten()
            .any(|w| w.kind == crate::protocol::WeaponKind::Lightning)
            && los_mechs.iter().any(|m| m.dist <= 45.0);
        let juke_boost = ((t <= self.world.juke_boost_until) || dodged_bullet)
            && me.energy > 45.0
            && !saving_for_lightning;

        let ground_boost = me.grounded
            && moving
            && ((!me.in_zone && me.energy > 5.0)
                || (aimed_laser_dodge && me.energy > 25.0)
                || (dodged_hazard && me.energy > 15.0)
                || juke_boost
                || (self.strategy.posture == Posture::EmergencyRegen
                    && los_mech_count > 0
                    && me.energy > 18.0)
                || (self.strategy.tactic == CombatTactic::CoverDuck
                    && los_mech_count > 0
                    && me.energy > 25.0)
                || (self.strategy.tactic == CombatTactic::CornerPeek && me.energy > 35.0)
                || (self.strategy.tactic == CombatTactic::PackKite && me.energy > 25.0)
                || (self.strategy.tactic == CombatTactic::AggressivePush
                    && me.energy > 45.0
                    && any_visible_mech
                        .as_ref()
                        .is_none_or(|m| !m.has_los || m.dist > 38.0))
                || (self.strategy.posture == Posture::ZoneRotate && me.energy > 25.0)
                || (self.strategy.posture == Posture::SupplyDrop && me.energy > 45.0)
                || (self.strategy.posture == Posture::LootWeapon
                    && slot1_empty
                    && me.energy > 28.0)
                || (self.strategy.posture == Posture::EarlyFarm && t < 45.0 && me.energy > 50.0));

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
        let tactic_tag = self.strategy.tactic.tag();
        let posture_str = if tactic_tag.is_empty() {
            self.strategy.posture.tag().to_string()
        } else {
            format!("{}:{}", self.strategy.posture.tag(), tactic_tag)
        };
        let mut label = format!(
            "{} hp={:.0} en={:.0} {} {}",
            posture_str, me.hp, me.energy, wtag, tgt_str
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
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;
    use std::process::{Command, Stdio};

    use super::*;
    use crate::combat::{eval_laser, slew_aligned, solve_gun_lead_airborne, TargetKind};
    use crate::geom::{angle_diff, direct_aim};
    use crate::protocol::{
        ActorObs, BeamObs, DropObs, GameEvent, ItemObs, Offer, OfferChoice, PickupObs,
        ProjectileObs, ServerMessage, SoundObs, WeaponKind, WeaponObs,
    };

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
            aim_z: 64.5,
            aim_half_h: 4.5,
            lead_vel: [0.0, 0.0, 0.0],
            radius: 2.5,
            hp: 1000.0,
            max_hp: 1400.0,
            weapons: vec![WeaponKind::Gun],
            charging: false,
            has_los: true,
            dist: 100.0,
            is_reloading: false,
            is_laser_on_cd: false,
            is_vulnerable: false,
        };
        let muzzle = [0.0, 0.0, 67.0];
        assert!(bot.map.has_los(muzzle, [100.0, 0.0, 64.5]));

        // 1. Mid-charge against healthy enemy -> keep holding
        let (_, hold1) = eval_laser(
            &bot.map,
            &bot.config,
            [0.0, 0.0],
            muzzle,
            0,
            &w_laser,
            &tgt_healthy,
            -0.7,
            1.3,
        );
        assert!(hold1, "should keep holding while charging at 0.20s");

        // 2. Full charge + aligned -> release (false)
        w_laser.charge = Some(0.72);
        let (_, hold2) = eval_laser(
            &bot.map,
            &bot.config,
            [0.0, -0.025],
            muzzle,
            0,
            &w_laser,
            &tgt_healthy,
            -0.7,
            1.3,
        );
        assert!(
            !hold2,
            "should release laser at >= 90% full charge when aligned"
        );

        // 3. Partial charge lethal with cookoff (+60%) + rear armor (x1.5) -> release early!
        w_laser.charge = Some(0.35);
        let mut tgt_mortar_rear = tgt_healthy.clone();
        tgt_mortar_rear.hp = 350.0;
        tgt_mortar_rear.weapons = vec![WeaponKind::Mortar];
        tgt_mortar_rear.aim = [0.0, 0.0]; // facing +x, muzzle at 0 is behind it
        let (_, hold3) = eval_laser(
            &bot.map,
            &bot.config,
            [0.0, -0.025],
            muzzle,
            0,
            &w_laser,
            &tgt_mortar_rear,
            -0.7,
            1.3,
        );
        assert!(
            !hold3,
            "should release partial charge for lethal mortar cookoff/rear finish"
        );

        // 4. Approaching hold_s forced timeout -> release cleanly
        w_laser.charge = Some(2.60);
        let (_, hold4) = eval_laser(
            &bot.map,
            &bot.config,
            [1.5, 0.0],
            muzzle,
            0,
            &w_laser,
            &tgt_healthy,
            -0.7,
            1.3,
        );
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
            aim_z: 64.5,
            aim_half_h: 4.5,
            lead_vel: [0.0, 0.0, 0.0],
            radius: 2.5,
            hp: 1000.0,
            max_hp: 1400.0,
            weapons: vec![WeaponKind::Gun],
            charging: false,
            has_los: true,
            dist: 100.0,
            is_reloading: false,
            is_laser_on_cd: false,
            is_vulnerable: false,
        };
        let muzzle = [0.0, 0.0, 67.0];
        // 0.15 rad (~8.6 deg) off target at 100 m must NOT release!
        let (_, hold) = eval_laser(
            &bot.map,
            &bot.config,
            [0.15, -0.025],
            muzzle,
            0,
            &w_laser,
            &tgt,
            -0.7,
            1.3,
        );
        assert!(hold, "must keep holding while 0.15 rad off target at 100 m");
    }

    #[test]
    fn finds_ground_pickup_on_sloped_terrain() {
        let (hello, mut obs) = load_sample_hello_and_obs();
        let mut bot = Bot::new(hello);
        obs.t = 5.0;
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
            bot.strategy.posture,
            Posture::LootWeapon,
            "must seek ground-level Laser pickup across sloped campus elevation"
        );
    }

    #[test]
    fn dynamic_upgrade_scorer_uses_config_and_held_weapons() {
        let (hello, obs) = load_sample_hello_and_obs();
        let bot = Bot::new(hello);
        let mut me = obs.self_obs.clone();
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
        assert_eq!(
            bot.strategy.pick_upgrade(&bot.config, &offer1, &me),
            1,
            "armor should win Tier S"
        );

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
            bot.strategy.pick_upgrade(&bot.config, &offer2, &me),
            1,
            "held gun_rate should beat unheld laser_damage"
        );

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
        assert_eq!(
            bot.strategy.pick_upgrade(&bot.config, &offer3, &me),
            1,
            "nanites should win"
        );
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
        assert!(bot.world.last_pickup_seen.contains_key(&8042));

        obs.t = 15.2;
        obs.events = vec![GameEvent::TakeFailed {
            id: 8042,
            reason: "gone".into(),
        }];
        bot.act(&obs);
        assert!(
            !bot.world.last_pickup_seen.contains_key(&8042),
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
        obs.t = 40.0;
        let mz = bot.map.ground_height(86.0, -110.0);
        obs.self_obs.pos = [86.0, -110.0, mz];
        obs.self_obs.hp = 380.0;
        obs.self_obs.max_hp = 1400.0;
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
        assert_eq!(bot.strategy.posture, Posture::EmergencyRegen);
        assert!(
            act1.fire.unwrap()[0],
            "should fire back while taking fire in LOS on way to cover"
        );

        obs.t = 40.3;
        obs.events.clear();
        obs.mechs.clear();
        let act2 = bot.act(&obs);
        assert_eq!(bot.strategy.posture, Posture::EmergencyRegen);
        assert_eq!(act2.fire.unwrap(), [false, false]);
        assert!(
            angle_diff(act2.aim.unwrap()[0], 2.4).abs() < 1e-6,
            "EmergencyRegen should shield rear toward recent damage bearing when hidden"
        );
    }

    #[test]
    fn regen_scans_360_when_hidden_behind_cover() {
        let (hello, mut obs) = load_sample_hello_and_obs();
        let mut bot = Bot::new(hello);
        obs.t = 40.0;
        let mz = bot.map.ground_height(86.0, -110.0);
        obs.self_obs.pos = [86.0, -110.0, mz];
        obs.self_obs.hp = 380.0;
        obs.self_obs.max_hp = 1400.0;
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
        bot.act(&obs);
        assert_eq!(bot.strategy.posture, Posture::EmergencyRegen);

        // Once hidden (> 2.2 s since damage, 0 LOS mechs, enemy still in tracked_enemies), must sweep 360 deg
        obs.t = 42.5;
        obs.events.clear();
        obs.mechs.clear();
        obs.self_obs.aim = [0.5, 0.0];
        let act2 = bot.act(&obs);
        assert_eq!(bot.strategy.posture, Posture::EmergencyRegen);
        assert_eq!(act2.fire.unwrap(), [false, false]);
        assert!(
            angle_diff(act2.aim.unwrap()[0], wrap_angle(0.5 + 0.95)).abs() < 1e-6,
            "hidden EmergencyRegen must sweep 360 deg instead of staring at 1 angle: got {:?}",
            act2.aim
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
    fn simultaneous_lightning_and_laser_firing_in_close_range() {
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

        let laser = WeaponObs {
            kind: WeaponKind::Laser,
            cooldown: 0.0,
            ammo: None,
            reload: None,
            charge: Some(0.0),
            stats: bot.config.weapons.laser.clone(),
        };
        let lt = WeaponObs {
            kind: WeaponKind::Lightning,
            cooldown: 0.0,
            ammo: None,
            reload: None,
            charge: None,
            stats: bot.config.weapons.lightning.clone(),
        };
        obs.self_obs.weapons = vec![Some(laser), Some(lt)];

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
        assert_eq!(
            act.fire.unwrap(),
            [true, true],
            "Lightning + Laser must fire Lightning AND start charging Laser simultaneously at <= 45 m"
        );
    }

    #[test]
    fn tracks_enemy_reload_sound_and_laser_beam_cooldown() {
        let (hello, mut obs) = load_sample_hello_and_obs();
        let mut bot = Bot::new(hello);
        obs.t = 20.0;
        let mz = bot.map.ground_height(0.0, 0.0);
        obs.self_obs.pos = [0.0, 0.0, mz];
        obs.self_obs.in_zone = true;
        obs.zone.center = [0.0, 0.0];
        obs.zone.radius = 400.0;
        obs.zone.next_center = [0.0, 0.0];
        obs.zone.next_radius = 350.0;
        obs.zone.shrink_start = 90.0;

        let ez = bot.map.ground_height(30.0, 0.0);
        obs.mechs = vec![ActorObs {
            id: 7,
            kind: "mech".into(),
            pos: [30.0, 0.0, ez],
            vel: [0.0, 0.0, 0.0],
            aim: [PI, 0.0],
            hp: Some(1400.0),
            max_hp: Some(1400.0),
            cubes: Some(0),
            weapons: vec![WeaponKind::Gun, WeaponKind::Laser],
            charging: true,
        }];
        bot.act(&obs);
        assert!(!bot.world.tracked_enemies[&7].is_reloading(20.0));
        assert!(!bot.world.tracked_enemies[&7].is_laser_on_cd(20.0));

        // Enemy releases laser + triggers reload sound
        obs.t = 20.1;
        obs.mechs[0].charging = false;
        obs.beams = vec![BeamObs {
            by: 7,
            kind: "laser".into(),
            path: vec![[30.0, 0.0, ez + 7.0], [0.0, 0.0, mz + 7.0]],
            age: 0.05,
        }];
        obs.sounds = vec![SoundObs {
            kind: "reload".into(),
            bearing: 0.04,
            dist: "near".into(),
            age: 0.05,
        }];
        bot.act(&obs);
        let e = &bot.world.tracked_enemies[&7];
        assert!(e.is_reloading(20.1), "reload sound should mark Gun reloading");
        assert!(e.is_laser_on_cd(20.1), "beam/release should mark Laser on cooldown");
        assert!(e.is_vulnerable(20.1), "enemy should register as vulnerable");

        // After both timers expire
        obs.t = 23.0;
        obs.beams.clear();
        obs.sounds.clear();
        bot.act(&obs);
        let e2 = &bot.world.tracked_enemies[&7];
        assert!(!e2.is_reloading(23.0));
        assert!(!e2.is_laser_on_cd(23.0));
    }

    #[test]
    fn tracks_enemy_gun_shots_to_predict_reload() {
        let (hello, mut obs) = load_sample_hello_and_obs();
        let mut bot = Bot::new(hello);
        let mz = bot.map.ground_height(0.0, 0.0);
        obs.self_obs.pos = [0.0, 0.0, mz];
        obs.self_obs.in_zone = true;
        obs.zone.center = [0.0, 0.0];
        obs.zone.radius = 400.0;
        obs.zone.next_center = [0.0, 0.0];
        obs.zone.next_radius = 350.0;
        obs.zone.shrink_start = 90.0;

        let ez = bot.map.ground_height(40.0, 0.0);
        obs.mechs = vec![ActorObs {
            id: 9,
            kind: "mech".into(),
            pos: [40.0, 0.0, ez],
            vel: [0.0, 0.0, 0.0],
            aim: [PI, 0.0],
            hp: Some(1400.0),
            max_hp: Some(1400.0),
            cubes: Some(0),
            weapons: vec![WeaponKind::Gun],
            charging: false,
        }];

        for i in 0..12u64 {
            obs.t = 30.0 + (i as f64) * 0.25;
            obs.projectiles = vec![ProjectileObs {
                id: 1000 + i,
                kind: "gun".into(),
                pos: [35.0, 0.0, ez + 7.0],
                vel: [-252.0, 0.0, 0.0],
            }];
            bot.act(&obs);
        }

        let e = &bot.world.tracked_enemies[&9];
        assert!(
            e.is_reloading(obs.t),
            "12 observed gun shots near enemy muzzle should predict full-mag reload"
        );
    }

    #[test]
    fn cover_reload_triggers_when_los_broken_behind_cover() {
        let (hello, mut obs) = load_sample_hello_and_obs();
        let mut bot = Bot::new(hello);
        let ez = bot.map.ground_height(75.0, -140.0);
        let threat = [75.0, -140.0, ez + 8.0];
        let start_z = bot.map.ground_height(86.0, -110.0);
        let cp = bot
            .nav
            .find_cover_point(
                &bot.map,
                [86.0, -110.0, start_z],
                8.0,
                &[threat],
                [0.0, 0.0],
                500.0,
                [0.0, 0.0],
                400.0,
            )
            .expect("cover near block 14 should exist");
        let cp_z = bot.map.ground_height(cp[0], cp[1]);

        obs.t = 40.0;
        obs.self_obs.pos = [cp[0], cp[1], cp_z];
        obs.self_obs.hp = 1400.0;
        obs.self_obs.max_hp = 1400.0;
        obs.self_obs.in_zone = true;
        obs.zone.center = [0.0, 0.0];
        obs.zone.radius = 400.0;
        obs.zone.next_center = [0.0, 0.0];
        obs.zone.next_radius = 350.0;
        obs.zone.shrink_start = 90.0;
        if let Some(Some(w)) = obs.self_obs.weapons.get_mut(0) {
            w.ammo = Some(4.0);
            w.reload = Some(0.0);
        }
        // Enemy tracked in 5s memory behind building wall (no LOS)
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

        let act = bot.act(&obs);
        assert_eq!(
            act.reload,
            Some(true),
            "should trigger cover reload at ammo=4 when LOS to nearby enemy is broken"
        );
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

    #[test]
    fn stays_aggressive_in_winning_1v1_instead_of_fleeing() {
        let (hello, mut obs) = load_sample_hello_and_obs();
        let mut bot = Bot::new(hello);
        obs.t = 40.0;
        let mz = bot.map.ground_height(0.0, 0.0);
        let ez = bot.map.ground_height(35.0, 0.0);
        obs.self_obs.pos = [0.0, 0.0, mz];
        obs.self_obs.hp = 500.0;
        obs.self_obs.max_hp = 1400.0;
        obs.self_obs.in_zone = true;
        obs.zone.center = [0.0, 0.0];
        obs.zone.radius = 400.0;
        obs.zone.next_center = [0.0, 0.0];
        obs.zone.next_radius = 350.0;
        obs.zone.shrink_start = 90.0;
        obs.self_obs.aim = direct_aim([0.0, 0.0, mz + 7.0], [35.0, 0.0, ez + 4.5]);
        obs.mechs = vec![ActorObs {
            id: 5,
            kind: "mech".into(),
            pos: [35.0, 0.0, ez],
            vel: [0.0, 0.0, 0.0],
            aim: [PI, 0.0],
            hp: Some(500.0),
            max_hp: Some(1400.0),
            cubes: Some(0),
            weapons: vec![WeaponKind::Gun],
            charging: false,
        }];

        let act = bot.act(&obs);
        assert_eq!(
            bot.strategy.posture,
            Posture::Combat,
            "winning/even 1v1 at 500 HP vs 500 HP must stay in Combat rather than fleeing"
        );
        assert!(
            act.fire.unwrap()[0],
            "must actively fire in winnable 1v1 duel"
        );
    }

    #[test]
    fn dives_into_lightning_range_when_holding_lightning() {
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

        let ez = bot.map.ground_height(55.0, 0.0);
        obs.mechs = vec![ActorObs {
            id: 4,
            kind: "mech".into(),
            pos: [55.0, 0.0, ez],
            vel: [0.0, 0.0, 0.0],
            aim: [PI, 0.0],
            hp: Some(1200.0),
            max_hp: Some(1400.0),
            cubes: Some(0),
            weapons: vec![WeaponKind::Gun],
            charging: false,
        }];

        let act = bot.act(&obs);
        let mv = act.move_dir.unwrap();
        assert_eq!(bot.strategy.tactic, CombatTactic::AggressivePush);
        assert!(
            mv[0] > 0.3,
            "holding Gun+Lightning at 55 m should push toward enemy into 30-45 m range, got {mv:?}"
        );
    }

    #[test]
    fn hunts_by_sound_when_no_mech_visible() {
        let (hello, mut obs) = load_sample_hello_and_obs();
        let mut bot = Bot::new(hello);
        obs.t = 40.0;
        let mz = bot.map.ground_height(0.0, 0.0);
        obs.self_obs.pos = [0.0, 0.0, mz];
        obs.self_obs.hp = 1400.0;
        obs.self_obs.max_hp = 1400.0;
        obs.self_obs.energy = 100.0;
        obs.self_obs.in_zone = true;
        obs.zone.center = [0.0, 0.0];
        obs.zone.radius = 400.0;
        obs.zone.next_center = [0.0, 0.0];
        obs.zone.next_radius = 350.0;
        obs.zone.shrink_start = 90.0;
        obs.mechs.clear();
        obs.crates.clear();
        obs.cubes.clear();
        obs.pickups.clear();
        obs.drops.clear();
        obs.sounds = vec![SoundObs {
            kind: "gun".into(),
            bearing: 0.0,
            dist: "near".into(),
            age: 0.1,
        }];

        let act = bot.act(&obs);
        let mv = act.move_dir.unwrap();
        assert_eq!(bot.strategy.posture, Posture::EarlyFarm);
        assert_eq!(bot.strategy.tactic, CombatTactic::AggressivePush);
        assert!(
            mv[0] > 0.4,
            "must actively hunt toward nearby gunfire sound, got {mv:?}"
        );
    }

    #[test]
    fn post_kill_regen_skips_crates_until_healed() {
        let (hello, mut obs) = load_sample_hello_and_obs();
        let mut bot = Bot::new(hello);
        bot.world.last_combat_t = 37.0;
        obs.t = 40.0;
        let mz = bot.map.ground_height(0.0, 0.0);
        let cz = bot.map.ground_height(15.0, 0.0);
        obs.self_obs.pos = [0.0, 0.0, mz];
        obs.self_obs.hp = 770.0; // 55% HP
        obs.self_obs.max_hp = 1400.0;
        obs.self_obs.aim = [0.0, 0.0];
        obs.self_obs.in_zone = true;
        obs.zone.center = [0.0, 0.0];
        obs.zone.radius = 400.0;
        obs.zone.next_center = [0.0, 0.0];
        obs.zone.next_radius = 350.0;
        obs.zone.shrink_start = 90.0;
        obs.mechs.clear();
        obs.pickups.clear();
        obs.cubes.clear();
        obs.drops.clear();
        obs.crates = vec![ItemObs {
            id: 1001,
            pos: [15.0, 0.0, cz],
            hp: Some(300.0),
        }];

        let act = bot.act(&obs);
        assert_eq!(
            act.fire.unwrap(),
            [false, false],
            "must not shoot crates and reset 5s regen delay while recovering at 55% HP"
        );
    }

    #[test]
    fn strafe_reverses_fast_on_jitter_and_gun_damage() {
        let (hello, mut obs) = load_sample_hello_and_obs();
        let mut bot = Bot::new(hello);
        obs.t = 10.0;
        obs.events.clear();
        bot.act(&obs);
        let initial_sign = bot.world.strafe_sign;

        // Auto jittered reversal must happen within <= 1.1 s
        let mut flipped_auto = false;
        for step in 1..=12 {
            obs.t = 10.0 + (step as f64) * 0.1;
            bot.act(&obs);
            if bot.world.strafe_sign != initial_sign {
                flipped_auto = true;
                break;
            }
        }
        assert!(
            flipped_auto,
            "strafe_sign must automatically reverse within 1.1 s"
        );

        // Wait >= 0.4 s since last flip, then incoming gun hit triggers immediate reverse + juke boost
        let flip_t = bot.world.last_strafe_flip_t;
        let pre_hit_sign = bot.world.strafe_sign;
        obs.t = flip_t + 0.40;
        obs.events = vec![GameEvent::Damaged {
            amount: 45.0,
            weapon: "gun".into(),
            bearing: Some(0.0),
            by: Some(5),
            rear: false,
        }];
        bot.act(&obs);
        assert_eq!(
            bot.world.strafe_sign, -pre_hit_sign,
            "gun damage >= 0.35 s after last flip must immediately reverse strafe"
        );
        assert!(
            bot.world.juke_boost_until > obs.t,
            "damage-triggered flip must arm brief juke boost"
        );

        // Another gun hit 0.1 s later must NOT double-flip back (0.35 s debounce)
        let after_hit_sign = bot.world.strafe_sign;
        obs.t += 0.10;
        bot.act(&obs);
        assert_eq!(
            bot.world.strafe_sign, after_hit_sign,
            "gun damage within 0.35 s must not double-flip"
        );
    }

    #[test]
    fn sidesteps_perpendicular_away_from_incoming_gun_bullet() {
        let (hello, mut obs) = load_sample_hello_and_obs();
        let bot = Bot::new(hello);
        let mz = bot.map.ground_height(0.0, 1.2);
        obs.t = 20.0;
        obs.self_obs.pos = [0.0, 1.2, mz];
        obs.self_obs.grounded = true;
        obs.projectiles = vec![ProjectileObs {
            id: 4001,
            kind: "gun".into(),
            pos: [-60.0, 0.0, mz + 4.5],
            vel: [252.0, 0.0, 0.0],
        }];

        let (mv, _, _, dodged_bullet) = steering::steer(
            &bot.map,
            &bot.nav,
            &bot.config,
            &obs,
            [1.0, 0.0],
            [0.0, 0.0],
            2.5,
            9.0,
            1.0,
        );
        assert!(dodged_bullet, "must detect incoming gun bullet threat");
        assert!(
            mv[1] > 0.5,
            "must sidestep +y perpendicular away from bullet ray at y=0 (me at y=1.2), got {mv:?}"
        );
    }

    #[test]
    fn ignores_gun_bullet_flying_high_overhead() {
        let (hello, mut obs) = load_sample_hello_and_obs();
        let bot = Bot::new(hello);
        let mz = bot.map.ground_height(0.0, 1.2);
        obs.t = 20.0;
        obs.self_obs.pos = [0.0, 1.2, mz];
        obs.self_obs.grounded = true;
        obs.projectiles = vec![ProjectileObs {
            id: 4002,
            kind: "gun".into(),
            pos: [-60.0, 0.0, mz + 25.0],
            vel: [252.0, 0.0, 0.0],
        }];

        let (_, _, _, dodged_bullet) = steering::steer(
            &bot.map,
            &bot.nav,
            &bot.config,
            &obs,
            [1.0, 0.0],
            [0.0, 0.0],
            2.5,
            9.0,
            1.0,
        );
        assert!(
            !dodged_bullet,
            "must ignore bullets flying high overhead (mz + 25 m)"
        );
    }

    #[test]
    fn focus_fires_wounded_sticky_target_over_full_hp_flanker() {
        let (_, obs) = load_sample_hello_and_obs();
        let me = &obs.self_obs;
        let wounded_sticky = TargetCandidate {
            id: 5,
            kind: TargetKind::Mech,
            pos: [65.0, 0.0, 0.0],
            vel: [0.0, 0.0, 0.0],
            est_accel_z: 0.0,
            aim: [PI, 0.0],
            com_z: 4.5,
            aim_z: 4.5,
            aim_half_h: 4.5,
            lead_vel: [0.0, 0.0, 0.0],
            radius: 2.5,
            hp: 500.0,
            max_hp: 1400.0,
            weapons: vec![WeaponKind::Gun],
            charging: false,
            has_los: true,
            dist: 65.0,
            is_reloading: false,
            is_laser_on_cd: false,
            is_vulnerable: false,
        };
        let full_flanker = TargetCandidate {
            id: 6,
            kind: TargetKind::Mech,
            pos: [40.0, 0.0, 0.0],
            vel: [0.0, 0.0, 0.0],
            est_accel_z: 0.0,
            aim: [PI, 0.0],
            com_z: 4.5,
            aim_z: 4.5,
            aim_half_h: 4.5,
            lead_vel: [0.0, 0.0, 0.0],
            radius: 2.5,
            hp: 1400.0,
            max_hp: 1400.0,
            weapons: vec![WeaponKind::Laser],
            charging: false,
            has_los: true,
            dist: 40.0,
            is_reloading: false,
            is_laser_on_cd: false,
            is_vulnerable: false,
        };
        let picked = pick_best_mech_target(&[full_flanker, wounded_sticky], me, None, Some(5))
            .expect("must pick a target");
        assert_eq!(
            picked.id, 5,
            "must focus-fire wounded sticky target (500 HP) over full-HP closer enemy"
        );
    }

    #[test]
    fn sweeps_nearby_power_cube_during_combat() {
        let (hello, mut obs) = load_sample_hello_and_obs();
        let mut bot = Bot::new(hello);
        obs.t = 40.0;
        let mz = bot.map.ground_height(0.0, 0.0);
        let cz = bot.map.ground_height(0.0, 10.0);
        let ez = bot.map.ground_height(55.0, 0.0);
        obs.self_obs.pos = [0.0, 0.0, mz];
        obs.self_obs.hp = 1400.0;
        obs.self_obs.max_hp = 1400.0;
        obs.self_obs.in_zone = true;
        obs.zone.center = [0.0, 0.0];
        obs.zone.radius = 400.0;
        obs.zone.next_center = [0.0, 0.0];
        obs.zone.next_radius = 350.0;
        obs.zone.shrink_start = 90.0;
        obs.self_obs.aim = direct_aim([0.0, 0.0, mz + 7.0], [55.0, 0.0, ez + 4.5]);
        obs.cubes = vec![ItemObs {
            id: 5001,
            pos: [0.0, 10.0, cz],
            hp: None,
        }];
        obs.mechs = vec![ActorObs {
            id: 4,
            kind: "mech".into(),
            pos: [55.0, 0.0, ez],
            vel: [0.0, 0.0, 0.0],
            aim: [PI, 0.0],
            hp: Some(1200.0),
            max_hp: Some(1400.0),
            cubes: Some(0),
            weapons: vec![WeaponKind::Gun],
            charging: false,
        }];

        let act = bot.act(&obs);
        let mv = act.move_dir.unwrap();
        assert_eq!(bot.strategy.posture, Posture::Combat);
        assert!(
            mv[1] > 0.4,
            "should sweep +y over nearby combat cube at (0, 10), got {mv:?}"
        );
        assert!(
            act.fire.unwrap()[0],
            "should keep shooting enemy while sweeping nearby cube"
        );
    }

    #[test]
    fn backpedal_kites_away_from_multi_enemy_pack() {
        let (hello, mut obs) = load_sample_hello_and_obs();
        let mut bot = Bot::new(hello);
        obs.t = 40.0;
        let mz = bot.map.ground_height(0.0, 0.0);
        let e1z = bot.map.ground_height(28.0, 6.0);
        let e2z = bot.map.ground_height(28.0, -6.0);
        assert!(bot.map.has_los([0.0, 0.0, mz + 7.0], [28.0, 6.0, e1z + 4.5]));
        assert!(bot.map.has_los([0.0, 0.0, mz + 7.0], [28.0, -6.0, e2z + 4.5]));
        obs.self_obs.pos = [0.0, 0.0, mz];
        obs.self_obs.hp = 1400.0;
        obs.self_obs.max_hp = 1400.0;
        obs.self_obs.energy = 100.0;
        obs.self_obs.in_zone = true;
        obs.zone.center = [0.0, 0.0];
        obs.zone.radius = 400.0;
        obs.zone.next_center = [0.0, 0.0];
        obs.zone.next_radius = 350.0;
        obs.zone.shrink_start = 90.0;
        obs.self_obs.aim = direct_aim([0.0, 0.0, mz + 7.0], [28.0, 6.0, e1z + 4.5]);
        obs.mechs = vec![
            ActorObs {
                id: 4,
                kind: "mech".into(),
                pos: [28.0, 6.0, e1z],
                vel: [0.0, 0.0, 0.0],
                aim: [PI, 0.0],
                hp: Some(1100.0),
                max_hp: Some(1400.0),
                cubes: Some(0),
                weapons: vec![WeaponKind::Gun],
                charging: false,
            },
            ActorObs {
                id: 5,
                kind: "mech".into(),
                pos: [28.0, -6.0, e2z],
                vel: [0.0, 0.0, 0.0],
                aim: [PI, 0.0],
                hp: Some(1400.0),
                max_hp: Some(1400.0),
                cubes: Some(0),
                weapons: vec![WeaponKind::Gun],
                charging: false,
            },
        ];

        let act = bot.act(&obs);
        let mv = act.move_dir.unwrap();
        assert_eq!(bot.strategy.tactic, CombatTactic::PackKite);
        assert!(
            mv[0] < -0.3,
            "should backpedal West (-x) away from 2-bot East pack, got {mv:?}"
        );
        assert!(act.fire.unwrap()[0], "must keep firing while pack-kiting");
    }

    #[test]
    fn avoids_laser_self_slow_when_enemy_aiming_charging_laser() {
        let (hello, mut obs) = load_sample_hello_and_obs();
        let mut bot = Bot::new(hello);
        obs.t = 45.0;
        let mz = bot.map.ground_height(0.0, 0.0);
        let ez = bot.map.ground_height(40.0, 0.0);
        obs.self_obs.pos = [0.0, 0.0, mz];
        obs.self_obs.hp = 1400.0;
        obs.self_obs.max_hp = 1400.0;
        obs.self_obs.energy = 100.0;
        obs.self_obs.aim = direct_aim([0.0, 0.0, mz + 7.0], [40.0, 0.0, ez + 4.5]);
        obs.self_obs.in_zone = true;
        obs.zone.center = [0.0, 0.0];
        obs.zone.radius = 400.0;
        obs.zone.next_center = [0.0, 0.0];
        obs.zone.next_radius = 350.0;
        obs.zone.shrink_start = 90.0;

        let gun = obs.self_obs.weapons[0].clone().unwrap();
        let laser = WeaponObs {
            kind: WeaponKind::Laser,
            cooldown: 0.0,
            ammo: None,
            reload: None,
            charge: Some(0.0),
            stats: bot.config.weapons.laser.clone(),
        };
        obs.self_obs.weapons = vec![Some(gun), Some(laser)];
        obs.mechs = vec![ActorObs {
            id: 4,
            kind: "mech".into(),
            pos: [40.0, 0.0, ez],
            vel: [0.0, 0.0, 0.0],
            aim: [PI, 0.0], // aimed directly at us
            hp: Some(1200.0),
            max_hp: Some(1400.0),
            cubes: Some(0),
            weapons: vec![WeaponKind::Laser],
            charging: true,
        }];

        let act = bot.act(&obs);
        assert_eq!(
            act.fire.unwrap(),
            [true, false],
            "must fire Gun without starting fresh Laser charge (0.5x self-slow) while enemy aims charging Laser at us"
        );
    }

    #[test]
    fn supply_drop_waits_for_eta_and_grabs_imminent_heal() {
        let (hello, mut obs) = load_sample_hello_and_obs();
        let mut bot = Bot::new(hello);
        obs.t = 70.0;
        let mz = bot.map.ground_height(0.0, 0.0);
        let dz = bot.map.ground_height(20.0, 0.0);
        obs.self_obs.pos = [0.0, 0.0, mz];
        obs.self_obs.hp = 900.0;
        obs.self_obs.max_hp = 1400.0;
        obs.self_obs.in_zone = true;
        obs.zone.center = [0.0, 0.0];
        obs.zone.radius = 400.0;
        obs.zone.next_center = [0.0, 0.0];
        obs.zone.next_radius = 350.0;
        obs.zone.shrink_start = 90.0;
        obs.mechs.clear();
        obs.drops = vec![DropObs {
            id: 20000,
            pos: [20.0, 0.0, dz],
            radius: 8.0,
            lands_in: 16.0,
            capture_s: 4.0,
            progress: 0.0,
            holder: None,
        }];

        bot.act(&obs);
        assert_ne!(
            bot.strategy.posture,
            Posture::SupplyDrop,
            "should NOT camp drop 16 s before landing when ETA is ~1.4 s"
        );

        obs.t = 83.0;
        obs.drops[0].lands_in = 3.0;
        bot.act(&obs);
        assert_eq!(
            bot.strategy.posture,
            Posture::SupplyDrop,
            "must contest imminent drop landing in 3.0 s"
        );
    }

    #[test]
    fn shoots_partly_hidden_target_peeking_over_wall() {
        let (hello, mut obs) = load_sample_hello_and_obs();
        let mut bot = Bot::new(hello);
        obs.t = 40.0;
        // Across isolated arena prism 25 (top = 4.23 m): center-of-mass blocked, top half clears top!
        let ez = -2.2;
        let me_pos = [-389.9, -287.2, 0.2];
        let muzzle = [me_pos[0], me_pos[1], me_pos[2] + 7.0];
        let com_z = ez + 4.5;
        let upper_z = ez + 0.82 * 9.0;
        assert!(
            !bot.map.has_los(muzzle, [-389.9, -244.2, com_z]),
            "center of mass across prism 25 must be blocked"
        );
        assert!(
            bot.map.has_los(muzzle, [-389.9, -244.2, upper_z]),
            "upper torso peeking over prism 25 must be clear"
        );

        obs.self_obs.pos = me_pos;
        obs.self_obs.hp = 1400.0;
        obs.self_obs.max_hp = 1400.0;
        obs.self_obs.in_zone = true;
        obs.zone.center = [-389.9, -265.0];
        obs.zone.radius = 400.0;
        obs.zone.next_center = [-389.9, -265.0];
        obs.zone.next_radius = 350.0;
        obs.zone.shrink_start = 90.0;
        obs.self_obs.aim = direct_aim(muzzle, [-389.9, -244.2, upper_z]);
        obs.mechs = vec![ActorObs {
            id: 5,
            kind: "mech".into(),
            pos: [-389.9, -244.2, ez],
            vel: [0.0, 0.0, 0.0],
            aim: [-1.5, 0.0],
            hp: Some(1200.0),
            max_hp: Some(1400.0),
            cubes: Some(0),
            weapons: vec![WeaponKind::Gun],
            charging: false,
        }];

        let (cands, _) = select_targets(&obs, &bot.map, &bot.config, &bot.world, muzzle);
        assert_eq!(cands.len(), 1);
        assert!(cands[0].has_los, "partially visible target must have LOS");
        assert!(
            (cands[0].aim_z - upper_z).abs() < 0.05,
            "aim_z should select exposed upper torso ({upper_z:.2}), got {:.2}",
            cands[0].aim_z
        );

        let act = bot.act(&obs);
        assert!(
            act.fire.unwrap()[0],
            "must shoot partly hidden mech whose top half peeks over wall"
        );
    }

    #[test]
    fn averages_velocity_for_reversing_juking_target() {
        let (hello, mut obs) = load_sample_hello_and_obs();
        let mut bot = Bot::new(hello);
        let mz = bot.map.ground_height(0.0, 0.0);
        let ez = bot.map.ground_height(35.0, 0.0);
        obs.self_obs.pos = [0.0, 0.0, mz];
        obs.self_obs.in_zone = true;
        obs.zone.center = [0.0, 0.0];
        obs.zone.radius = 400.0;
        obs.zone.next_center = [0.0, 0.0];
        obs.zone.next_radius = 350.0;
        obs.zone.shrink_start = 90.0;

        // 1. Alternating A-D reversing target (+12 / -12 m/s every 0.2 s)
        for i in 0..6u64 {
            obs.t = 20.0 + (i as f64) * 0.20;
            let vy = if i % 2 == 0 { 12.0 } else { -12.0 };
            obs.mechs = vec![ActorObs {
                id: 5,
                kind: "mech".into(),
                pos: [35.0, 0.0, ez],
                vel: [0.0, vy, 0.0],
                aim: [PI, 0.0],
                hp: Some(1400.0),
                max_hp: Some(1400.0),
                cubes: Some(0),
                weapons: vec![WeaponKind::Gun],
                charging: false,
            }];
            bot.act(&obs);
        }
        let juker = &bot.world.tracked_enemies[&5];
        assert!(
            juker.juke_score > 0.5,
            "reversing enemy must raise juke_score, got {}",
            juker.juke_score
        );
        let eff_juke = juker.effective_lead_vel();
        assert!(
            eff_juke[1].abs() < 8.0,
            "juker effective lead velocity should damp below raw 12.0, got {:?}",
            eff_juke
        );

        // 2. Steady-running enemy keeps full velocity lead
        for i in 0..6u64 {
            obs.t = 30.0 + (i as f64) * 0.20;
            obs.mechs = vec![ActorObs {
                id: 6,
                kind: "mech".into(),
                pos: [35.0, 0.0, ez],
                vel: [0.0, 12.0, 0.0],
                aim: [PI, 0.0],
                hp: Some(1400.0),
                max_hp: Some(1400.0),
                cubes: Some(0),
                weapons: vec![WeaponKind::Gun],
                charging: false,
            }];
            bot.act(&obs);
        }
        let steady = &bot.world.tracked_enemies[&6];
        assert_eq!(steady.juke_score, 0.0);
        assert!(
            (steady.effective_lead_vel()[1] - 12.0).abs() < 1e-9,
            "steady target must use full 12.0 m/s lead velocity"
        );
    }
}

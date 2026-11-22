use std::f64::consts::PI;

use crate::geom::{dist2d, unit2d};
use crate::map::Map;
use crate::nav::NavGrid;
use crate::protocol::{stat, Config, Observation};

#[allow(clippy::too_many_arguments)]
pub(crate) fn steer(
    map: &Map,
    nav: &NavGrid,
    config: &Config,
    obs: &Observation,
    goal_dir: [f64; 2],
    last_move: [f64; 2],
    mech_radius: f64,
    mech_height: f64,
    strafe_sign: f64,
    climbing_roof: bool,
    ducking_to_cover: bool,
) -> ([f64; 2], bool, bool, bool) {
    let me = &obs.self_obs;
    let step_up = stat(&me.stats, "step_up", 1.2);
    let max_slope_rad = stat(&me.stats, "max_slope_deg", 50.0).to_radians();
    let max_slope_tan = max_slope_rad.tan();
    let mortar_g = stat(&config.weapons.mortar, "gravity", 22.0);
    let splash_r = stat(&config.weapons.mortar, "splash_radius", 7.0);
    let accel = if me.grounded {
        stat(&me.stats, "ground_accel", 58.5)
    } else {
        stat(&me.stats, "air_accel", 12.0)
    };
    let move_spd = stat(&me.stats, "boost_speed", 19.5);

    let mut mortar_threats: Vec<[f64; 2]> = Vec::new();
    let mut bullet_escapes: Vec<([f64; 2], f64)> = Vec::new();
    let mut bullet_needs_boost = false;
    for proj in &obs.projectiles {
        if proj.kind == "mortar" {
            if let Some(impact) = predict_mortar_impact(map, proj.pos, proj.vel, mortar_g) {
                if dist2d(me.pos[0], me.pos[1], impact[0], impact[1]) <= splash_r + 10.0 {
                    mortar_threats.push(impact);
                }
            }
        } else if proj.kind == "gun" || proj.kind == "npc" {
            let speed_xy = proj.vel[0].hypot(proj.vel[1]);
            if speed_xy <= 30.0 {
                continue;
            }
            let dir_p = [proj.vel[0] / speed_xy, proj.vel[1] / speed_xy];
            let rel = [me.pos[0] - proj.pos[0], me.pos[1] - proj.pos[1]];
            let along = rel[0] * dir_p[0] + rel[1] * dir_p[1];
            if along <= 0.0 {
                continue;
            }
            let t_hit = along / speed_xy;
            if !(0.06..=0.65).contains(&t_hit) {
                continue;
            }
            let bz = proj.pos[2] + proj.vel[2] * t_hit;
            if bz < me.pos[2] - 1.0 || bz > me.pos[2] + mech_height + 1.5 {
                continue;
            }
            let perp_left = [-dir_p[1], dir_p[0]];
            let signed_perp = rel[0] * perp_left[0] + rel[1] * perp_left[1];
            if signed_perp.abs() > mech_radius + 2.2 {
                continue;
            }
            let vel_perp = me.vel[0] * perp_left[0] + me.vel[1] * perp_left[1];
            let pos_keep = if vel_perp.abs() >= 5.0 {
                (signed_perp + kinematic_disp(vel_perp, vel_perp.signum() * move_spd, accel, t_hit))
                    .abs()
            } else {
                0.0
            };
            let (sign, urgency, triggers_boost) = if vel_perp.abs() >= 5.0
                && pos_keep >= mech_radius + 0.85
            {
                (vel_perp.signum(), 0.75, pos_keep < mech_radius + 1.2)
            } else {
                let pos_plus =
                    (signed_perp + kinematic_disp(vel_perp, move_spd, accel, t_hit)).abs();
                let pos_minus =
                    (signed_perp + kinematic_disp(vel_perp, -move_spd, accel, t_hit)).abs();
                let s = if (pos_plus - pos_minus).abs() < 1e-6 {
                    if signed_perp.abs() < 0.25 {
                        strafe_sign
                    } else {
                        signed_perp.signum()
                    }
                } else if pos_plus >= pos_minus {
                    1.0
                } else {
                    -1.0
                };
                let urg = 1.65 * (1.0 - (t_hit / 0.65).clamp(0.0, 0.7));
                (s, urg, true)
            };
            if triggers_boost {
                bullet_needs_boost = true;
            }
            let esc = [perp_left[0] * sign, perp_left[1] * sign];
            bullet_escapes.push((esc, urgency));
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
    let dodged_bullet = bullet_needs_boost;

    if goal_dir[0].hypot(goal_dir[1]) < 0.05 && !dodged_hazard && bullet_escapes.is_empty() {
        return ([0.0, 0.0], false, false, false);
    }

    let on_ground_here =
        me.grounded && (me.pos[2] - map.ground_height(me.pos[0], me.pos[1])).abs() <= 2.0;

    let mut best_dir = goal_dir;
    let mut best_score = f64::NEG_INFINITY;

    for k in 0..16 {
        let theta = (k as f64) * (PI / 8.0);
        let dir = [theta.cos(), theta.sin()];
        let goal_align = dir[0] * goal_dir[0] + dir[1] * goal_dir[1];
        let mut score = goal_align;
        score += 0.18 * (dir[0] * last_move[0] + dir[1] * last_move[1]);

        let check_step = |dist_m: f64| -> bool {
            let nx = me.pos[0] + dir[0] * dist_m;
            let ny = me.pos[1] + dir[1] * dist_m;
            let g_next = map.ground_height(nx, ny);
            let dz_g = g_next - me.pos[2];
            if on_ground_here && dz_g > step_up && dz_g / dist_m > max_slope_tan {
                return false;
            }
            let probe_z = if on_ground_here { g_next } else { me.pos[2] };
            let (ok, surf_z) = map.is_pos_walkable(
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

        if !(climbing_roof && goal_align > 0.6) {
            if !ok_near {
                score -= 4.0;
            } else if !ok_mid {
                score -= if ducking_to_cover { 0.6 } else { 1.8 };
            } else if !ok_far {
                score -= if ducking_to_cover { 0.15 } else { 0.6 };
            }
        }

        for imp in &mortar_threats {
            let away = unit2d(me.pos[0] - imp[0], me.pos[1] - imp[1]);
            score += 2.2 * (dir[0] * away[0] + dir[1] * away[1]);
        }

        let laser_w = if ducking_to_cover { 0.0 } else { 3.5 };
        for ray in &laser_threats {
            let cross = (dir[0] * ray[1] - dir[1] * ray[0]).abs();
            score += laser_w * cross;
        }

        for &(esc, w) in &bullet_escapes {
            score += w * (dir[0] * esc[0] + dir[1] * esc[1]);
        }

        if aimed_laser_dodge && ok_near {
            let nx3 = me.pos[0] + dir[0] * 3.5;
            let ny3 = me.pos[1] + dir[1] * 3.5;
            let (ncx, ncy) = nav.world_to_cell(nx3, ny3);
            if nav.near_cover[ncy * nav.nx + ncx] {
                score += if ducking_to_cover { 0.40 } else { 0.85 };
            }
        }

        if score > best_score {
            best_score = score;
            best_dir = dir;
        }
    }

    (best_dir, dodged_hazard, aimed_laser_dodge, dodged_bullet)
}

fn kinematic_disp(v0: f64, v_cmd: f64, accel: f64, dt: f64) -> f64 {
    let dv = v_cmd - v0;
    let t_acc = (dv.abs() / accel.max(1.0)).min(dt);
    let v1 = v0 + dv.signum() * accel * t_acc;
    0.5 * (v0 + v1) * t_acc + v1 * (dt - t_acc)
}

fn predict_mortar_impact(map: &Map, pos: [f64; 3], vel: [f64; 3], g: f64) -> Option<[f64; 2]> {
    let mut t = 0.2;
    for _ in 0..14 {
        let x = pos[0] + vel[0] * t;
        let y = pos[1] + vel[1] * t;
        let z = pos[2] + vel[2] * t - 0.5 * g * t * t;
        let surf = map.surface_height(x, y, z + 5.0);
        if z <= surf {
            return Some([x, y]);
        }
        t += 0.25;
    }
    None
}

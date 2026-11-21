#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[inline]
pub fn stat(map: &HashMap<String, f64>, key: &str, fallback: f64) -> f64 {
    map.get(key).copied().unwrap_or(fallback)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WeaponKind {
    Gun,
    Mortar,
    Laser,
    Lightning,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Hello(Box<Hello>),
    Obs(Box<Observation>),
    End(EndMsg),
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Hello {
    pub protocol: String,
    pub slot: u64,
    pub seed: u64,
    pub map: HelloMap,
    pub config: Config,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HelloMap {
    pub dir: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(rename = "match")]
    pub match_cfg: HashMap<String, f64>,
    pub bot: HashMap<String, f64>,
    pub mech: HashMap<String, f64>,
    pub vision: ConfigVision,
    pub sound: ConfigSound,
    pub weapons: ConfigWeapons,
    #[serde(default)]
    pub projectile: HashMap<String, f64>,
    pub cubes: ConfigCubes,
    #[serde(default)]
    pub crates: HashMap<String, f64>,
    pub loot: ConfigLoot,
    #[serde(default)]
    pub npc: HashMap<String, f64>,
    #[serde(default)]
    pub drops: Option<ConfigDrops>,
    pub zone: ConfigZone,
    #[serde(default)]
    pub replay: HashMap<String, f64>,
    pub debug: DebugCaps,
    #[serde(default)]
    pub upgrades: Vec<UpgradeDef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConfigVision {
    pub range: f64,
    pub fov_deg: f64,
    pub near_range: f64,
    pub canopy_blocks: bool,
    pub show_enemy_hp: bool,
    #[serde(default)]
    pub memory_s: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConfigSound {
    pub bearing_noise_deg: f64,
    pub near: f64,
    pub mid: f64,
    pub step_speed: f64,
    pub step_every: f64,
    #[serde(default)]
    pub range: HashMap<String, f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConfigWeapons {
    pub gun: HashMap<String, f64>,
    pub mortar: HashMap<String, f64>,
    pub laser: HashMap<String, f64>,
    pub lightning: HashMap<String, f64>,
}

impl ConfigWeapons {
    pub fn for_kind(&self, kind: WeaponKind) -> &HashMap<String, f64> {
        match kind {
            WeaponKind::Gun => &self.gun,
            WeaponKind::Mortar => &self.mortar,
            WeaponKind::Laser => &self.laser,
            WeaponKind::Lightning => &self.lightning,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConfigCubes {
    #[serde(default = "default_cube_pickup_radius")]
    pub pickup_radius: f64,
    #[serde(default = "default_hp_per_cube")]
    pub hp_per_cube: f64,
    #[serde(default = "default_damage_per_cube")]
    pub damage_per_cube: f64,
    #[serde(default = "default_stat_cap")]
    pub stat_cap: f64,
    #[serde(default = "default_death_drop_extra")]
    pub death_drop_extra: f64,
    #[serde(default = "default_size_per_cube")]
    pub size_per_cube: f64,
    #[serde(default)]
    pub upgrade_thresholds: Vec<f64>,
}

fn default_cube_pickup_radius() -> f64 {
    3.5
}
fn default_hp_per_cube() -> f64 {
    0.04
}
fn default_damage_per_cube() -> f64 {
    0.04
}
fn default_stat_cap() -> f64 {
    10.0
}
fn default_death_drop_extra() -> f64 {
    2.0
}
fn default_size_per_cube() -> f64 {
    0.05
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConfigLoot {
    pub pickup_radius: f64,
    pub equip_s: f64,
    pub floor_count: f64,
    pub floor_roof_fraction: f64,
    pub crate_weapon_chance: f64,
    pub death_drop: f64,
    #[serde(default)]
    pub weights: HashMap<String, f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConfigDrops {
    pub times: Vec<f64>,
    pub fall_s: f64,
    pub radius: f64,
    pub capture_s: f64,
    pub cubes: f64,
    pub repair: f64,
    pub spread: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConfigZone {
    pub start_center: [f64; 2],
    pub start_radius: f64,
    pub phases: Vec<ZonePhase>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ZonePhase {
    pub hold_s: f64,
    pub shrink_s: f64,
    pub radius: f64,
    pub dps: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DebugCaps {
    pub max_lines: usize,
    pub max_points: usize,
    pub max_text: usize,
    pub max_text_chars: usize,
    pub max_bytes: usize,
    pub max_match_bytes: usize,
    pub hold_s: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpgradeDef {
    pub id: String,
    pub name: String,
    pub desc: String,
    #[serde(default)]
    pub requires: Option<WeaponKind>,
    pub max: f64,
    #[serde(default)]
    pub effects: Vec<UpgradeEffect>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpgradeEffect {
    pub path: String,
    #[serde(default)]
    pub mul: Option<f64>,
    #[serde(default)]
    pub add: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Observation {
    pub tick: u64,
    pub t: f64,
    pub alive: u64,
    #[serde(rename = "self")]
    pub self_obs: SelfObs,
    #[serde(default)]
    pub mechs: Vec<ActorObs>,
    #[serde(default)]
    pub projectiles: Vec<ProjectileObs>,
    #[serde(default)]
    pub crates: Vec<ItemObs>,
    #[serde(default)]
    pub cubes: Vec<ItemObs>,
    #[serde(default)]
    pub pickups: Vec<PickupObs>,
    #[serde(default)]
    pub sounds: Vec<SoundObs>,
    #[serde(default)]
    pub beams: Vec<BeamObs>,
    #[serde(default)]
    pub events: Vec<GameEvent>,
    pub zone: ZoneObs,
    #[serde(default)]
    pub offer: Option<Offer>,
    #[serde(default)]
    pub drops: Vec<DropObs>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SelfObs {
    pub id: u64,
    pub pos: [f64; 3],
    pub vel: [f64; 3],
    pub aim: [f64; 2],
    pub hp: f64,
    pub max_hp: f64,
    pub energy: f64,
    pub max_energy: f64,
    pub grounded: bool,
    pub cubes: u64,
    #[serde(default)]
    pub upgrades: Vec<String>,
    pub weapons: Vec<Option<WeaponObs>>,
    pub in_zone: bool,
    #[serde(default)]
    pub stats: HashMap<String, f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WeaponObs {
    pub kind: WeaponKind,
    pub cooldown: f64,
    #[serde(default)]
    pub ammo: Option<f64>,
    #[serde(default)]
    pub reload: Option<f64>,
    #[serde(default)]
    pub charge: Option<f64>,
    #[serde(default)]
    pub stats: HashMap<String, f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActorObs {
    pub id: u64,
    pub kind: String,
    pub pos: [f64; 3],
    pub vel: [f64; 3],
    pub aim: [f64; 2],
    #[serde(default)]
    pub hp: Option<f64>,
    #[serde(default)]
    pub max_hp: Option<f64>,
    #[serde(default)]
    pub cubes: Option<u64>,
    #[serde(default)]
    pub weapons: Vec<WeaponKind>,
    #[serde(default)]
    pub charging: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectileObs {
    pub id: u64,
    pub kind: String,
    pub pos: [f64; 3],
    pub vel: [f64; 3],
}

#[derive(Debug, Clone, Deserialize)]
pub struct ItemObs {
    pub id: u64,
    pub pos: [f64; 3],
    #[serde(default)]
    pub hp: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PickupObs {
    pub id: u64,
    pub kind: WeaponKind,
    pub pos: [f64; 3],
}

#[derive(Debug, Clone, Deserialize)]
pub struct SoundObs {
    pub kind: String,
    pub bearing: f64,
    pub dist: String,
    pub age: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BeamObs {
    pub by: u64,
    pub kind: String,
    pub path: Vec<[f64; 3]>,
    pub age: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DropObs {
    pub id: u64,
    pub pos: [f64; 3],
    pub radius: f64,
    pub lands_in: f64,
    pub capture_s: f64,
    pub progress: f64,
    pub holder: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ZoneObs {
    pub center: [f64; 2],
    pub radius: f64,
    pub next_center: [f64; 2],
    pub next_radius: f64,
    pub shrink_start: f64,
    pub shrink_end: f64,
    pub dps: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Offer {
    pub id: u64,
    pub choices: Vec<OfferChoice>,
    pub expires: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OfferChoice {
    pub id: String,
    pub name: String,
    pub desc: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GameEvent {
    Damaged {
        amount: f64,
        weapon: String,
        bearing: Option<f64>,
        by: Option<u64>,
        #[serde(default)]
        rear: bool,
    },
    Hit {
        target: u64,
        amount: f64,
        weapon: String,
        #[serde(default)]
        effect: Option<String>,
        #[serde(default)]
        rear: bool,
    },
    Effect {
        effect: String,
        by: Option<u64>,
    },
    Drop {
        id: u64,
        cubes: u64,
    },
    Kill {
        target: u64,
    },
    Pickup {
        cubes: u64,
    },
    Upgrade {
        id: String,
    },
    Took {
        id: u64,
        kind: WeaponKind,
        slot: usize,
    },
    Dropped {
        id: u64,
        kind: WeaponKind,
        slot: usize,
    },
    TakeFailed {
        id: u64,
        reason: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EndMsg {
    pub place: u64,
    pub kills: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Ready<'a> {
    #[serde(rename = "type")]
    pub msg_type: &'static str,
    pub name: &'a str,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Action {
    pub tick: u64,
    #[serde(rename = "move", skip_serializing_if = "Option::is_none")]
    pub move_dir: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boost: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jump: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aim: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fire: Option<[bool; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reload: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pick: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub take: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drop: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug: Option<DebugDraw>,
}

impl Action {
    pub fn sanitize(&mut self, min_pitch: f64, max_pitch: f64, fallback_aim: [f64; 2]) {
        if let Some(ref mut mv) = self.move_dir {
            if !mv[0].is_finite() || !mv[1].is_finite() {
                *mv = [0.0, 0.0];
            } else {
                let len = mv[0].hypot(mv[1]);
                if len > 1.0 {
                    mv[0] /= len;
                    mv[1] /= len;
                }
            }
        }
        if let Some(ref mut aim) = self.aim {
            if !aim[0].is_finite() {
                aim[0] = fallback_aim[0];
            }
            if !aim[1].is_finite() {
                aim[1] = fallback_aim[1];
            }
            aim[1] = aim[1].clamp(min_pitch, max_pitch);
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DebugDraw {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<Vec<DebugLine>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub points: Option<Vec<DebugPoint>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<Vec<DebugText>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DebugLine {
    pub a: [f64; 3],
    pub b: [f64; 3],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DebugPoint {
    pub at: [f64; 3],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DebugText {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<&'static str>,
}

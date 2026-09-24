//! Simulated vehicle state with smoothly varying physical values.

use protocol::{encode_frame, Catalog, RawFrame, SignalValue};

use crate::rng::Rng;

/// Length of one drive cycle: park, drive (ramp up, cruise, ramp down), park.
pub const DRIVE_CYCLE_S: f64 = 60.0;

/// Signal names the simulator knows how to generate.
pub const KNOWN_SIGNALS: [&str; 6] = [
    "vehicle_speed",
    "battery_soc",
    "motor_temp",
    "gear",
    "door_open",
    "cabin_temp",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gear {
    P,
    D,
}

impl Gear {
    fn label(self) -> &'static str {
        match self {
            Gear::P => "P",
            Gear::D => "D",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Behaviour {
    Drive,
    Park,
}

#[derive(Debug, Clone)]
pub struct Vehicle {
    behaviour: Behaviour,
    t_s: f64,
    pub speed_kmh: f64,
    pub soc_pct: f64,
    pub motor_temp_c: f64,
    pub cabin_temp_c: f64,
    pub gear: Gear,
    pub door_open: bool,
}

/// Smooth 0 -> 1 ramp for `x` in `[0, 1]`.
fn smoothstep(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

impl Vehicle {
    pub fn new(behaviour: Behaviour) -> Self {
        Vehicle {
            behaviour,
            t_s: 0.0,
            speed_kmh: 0.0,
            soc_pct: 80.0,
            motor_temp_c: 25.0,
            cabin_temp_c: 21.0,
            gear: Gear::P,
            door_open: false,
        }
    }

    /// Advances the simulation by `dt_s` seconds.
    pub fn step(&mut self, dt_s: f64, rng: &mut Rng) {
        self.t_s += dt_s;
        match self.behaviour {
            Behaviour::Drive => self.step_drive(rng),
            Behaviour::Park => {
                self.gear = Gear::P;
                self.speed_kmh = 0.0;
            }
        }
        // Motor temperature follows a first-order lag toward a speed-dependent
        // target: ~96 C at cruise, ambient when stopped.
        let target_temp = 25.0 + self.speed_kmh * 0.65;
        self.motor_temp_c += (target_temp - self.motor_temp_c) * (dt_s / 8.0).min(1.0);
        self.soc_pct = (self.soc_pct - dt_s * (0.0002 + self.speed_kmh * 0.00015)).max(5.0);
        self.cabin_temp_c = 21.0 + 1.5 * (self.t_s / 40.0).sin() + rng.noise(0.05);

        if self.gear == Gear::P {
            let toggles_per_s = if self.behaviour == Behaviour::Park {
                0.15
            } else {
                0.1
            };
            if rng.chance(toggles_per_s * dt_s) {
                self.door_open = !self.door_open;
            }
        } else {
            self.door_open = false;
        }
    }

    fn step_drive(&mut self, rng: &mut Rng) {
        let phase = self.t_s % DRIVE_CYCLE_S;
        self.gear = if (3.0..52.0).contains(&phase) {
            Gear::D
        } else {
            Gear::P
        };
        // Ramp up 5..15 s, cruise with gentle variation, ramp down 37..47 s.
        let envelope = smoothstep((phase - 5.0) / 10.0).min(smoothstep((47.0 - phase) / 10.0));
        let target = envelope * (110.0 + 8.0 * (phase * 0.5).sin());
        self.speed_kmh = if envelope > 0.0 {
            (target + rng.noise(0.3)).max(0.0)
        } else {
            0.0
        };
    }

    /// Current physical value for a catalog signal name, if simulated.
    pub fn value(&self, name: &str) -> Option<SignalValue> {
        Some(match name {
            "vehicle_speed" => SignalValue::Num(self.speed_kmh),
            "battery_soc" => SignalValue::Num(self.soc_pct),
            "motor_temp" => SignalValue::Num(self.motor_temp_c),
            "cabin_temp" => SignalValue::Num(self.cabin_temp_c),
            "gear" => SignalValue::Enum(self.gear.label().to_owned()),
            "door_open" => SignalValue::Bool(self.door_open),
            _ => return None,
        })
    }

    /// Encodes the current value of every simulated catalog signal.
    pub fn frames(
        &self,
        catalog: &Catalog,
        timestamp_us: u64,
    ) -> Result<Vec<RawFrame>, protocol::EncodeError> {
        catalog
            .signals()
            .filter_map(|s| {
                self.value(s.name())
                    .map(|v| encode_frame(s, timestamp_us, &v))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::decode_packet;

    const CATALOGS: [&str; 2] = [
        include_str!("../../../catalogs/r1s.json"),
        include_str!("../../../catalogs/r2.json"),
    ];
    const DT: f64 = 1.0 / 50.0;

    fn run(behaviour: Behaviour, seconds: f64) -> Vec<Vehicle> {
        let mut rng = Rng::new(42);
        let mut v = Vehicle::new(behaviour);
        (0..(seconds / DT) as usize)
            .map(|_| {
                v.step(DT, &mut rng);
                v.clone()
            })
            .collect()
    }

    #[test]
    fn drive_cycle_shape() {
        let states = run(Behaviour::Drive, 2.0 * DRIVE_CYCLE_S);
        let max_speed = states.iter().map(|s| s.speed_kmh).fold(0.0, f64::max);
        assert!((100.0..=125.0).contains(&max_speed), "{max_speed}");
        assert!(states
            .iter()
            .any(|s| s.gear == Gear::D && s.motor_temp_c > 90.0));
        assert!(states
            .iter()
            .all(|s| s.gear == Gear::D || s.speed_kmh == 0.0));
        assert!(states.iter().all(|s| s.gear == Gear::P || !s.door_open));
        assert!(states.last().unwrap().soc_pct < 80.0);
        // Gear goes P -> D -> P within one cycle.
        let gears: Vec<Gear> = states.iter().map(|s| s.gear).collect();
        let changes = gears.windows(2).filter(|w| w[0] != w[1]).count();
        assert_eq!(changes, 4, "two P->D->P cycles");
    }

    #[test]
    fn speed_is_smooth() {
        let states = run(Behaviour::Drive, DRIVE_CYCLE_S);
        let max_jump = states
            .windows(2)
            .map(|w| (w[1].speed_kmh - w[0].speed_kmh).abs())
            .fold(0.0, f64::max);
        assert!(max_jump < 2.0, "speed jumped {max_jump} km/h in one tick");
    }

    #[test]
    fn park_stays_parked_and_toggles_door() {
        let states = run(Behaviour::Park, 120.0);
        assert!(states
            .iter()
            .all(|s| s.gear == Gear::P && s.speed_kmh == 0.0));
        assert!(states.iter().any(|s| s.door_open));
        assert!(states.iter().any(|s| !s.door_open));
    }

    #[test]
    fn every_state_encodes_and_decodes_for_both_catalogs() {
        for json in CATALOGS {
            let cat = Catalog::from_json(json).unwrap();
            for behaviour in [Behaviour::Drive, Behaviour::Park] {
                for (i, state) in run(behaviour, DRIVE_CYCLE_S).iter().enumerate() {
                    let frames = state.frames(&cat, i as u64).unwrap();
                    assert_eq!(frames.len(), cat.signals().count());
                    let bytes = protocol::encode_packet(&frames).unwrap();
                    let d = decode_packet(&bytes, &cat).unwrap();
                    assert!(d.errors.is_empty() && d.unknown_signals == 0);
                    let speed = d
                        .samples
                        .iter()
                        .find(|s| s.name == "vehicle_speed")
                        .and_then(|s| s.value.as_num())
                        .unwrap();
                    assert!((speed - state.speed_kmh).abs() < 0.01);
                }
            }
        }
    }
}

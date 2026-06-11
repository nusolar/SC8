//! Decodes radio-delivered CAN frames into the dashboard model, using the
//! same dbc-codegen modules the car uses (reused from solar_rust via #[path]
//! in main.rs — single source of truth, no socketcan needed).

use embedded_can::{ExtendedId, Id, StandardId};
use telemetry_types::CanFrameMsg;

use crate::{messages_kelly, messages_mppt, messages_mppt_2, messages_mppt_3};

// ─── Model (mirrors solar_rust/src/dashboard.rs) ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum MotorDirection {
    Forward,
    #[default]
    Neutral,
    Reverse,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum MotorFeedback {
    #[default]
    Stationary,
    Forward,
    Backward,
    Unknown,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct KellyFaults {
    pub over_voltage: bool,
    pub low_voltage: bool,
    pub stall: bool,
    pub internal_volts: bool,
    pub controller_over_temp: bool,
    pub motor_over_temp: bool,
    pub throttle_error: bool,
    pub hall_throttle_open: bool,
    pub angle_sensor: bool,
    pub hall_galvanometer: bool,
    pub id_error: bool,
}

impl KellyFaults {
    pub fn rows(&self) -> [(&'static str, bool); 11] {
        [
            ("Over Voltage", self.over_voltage),
            ("Low Voltage", self.low_voltage),
            ("Stall", self.stall),
            ("Internal Volts", self.internal_volts),
            ("Ctrl Over Temp", self.controller_over_temp),
            ("Motor Over Temp", self.motor_over_temp),
            ("Throttle Error", self.throttle_error),
            ("Hall Throttle", self.hall_throttle_open),
            ("Angle Sensor", self.angle_sensor),
            ("Hall Galvo", self.hall_galvanometer),
            ("ID Error", self.id_error),
        ]
    }
    pub fn any(&self) -> bool {
        self.rows().iter().any(|(_, a)| *a)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MpptData {
    pub input_voltage: f32,
    pub input_current: f32,
    pub output_voltage: f32,
    pub output_current: f32,
    pub mosfet_temp: f32,
    pub fault: bool,
    pub limiting: bool,
    pub aux_fault: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MpptStatus {
    Ok,
    Limiting,
    Fault,
}

impl MpptData {
    pub fn watts_in(&self) -> f32 {
        self.input_voltage * self.input_current
    }
    pub fn watts_out(&self) -> f32 {
        self.output_voltage * self.output_current
    }
    pub fn status(&self) -> MpptStatus {
        if self.fault || self.aux_fault {
            MpptStatus::Fault
        } else if self.limiting {
            MpptStatus::Limiting
        } else {
            MpptStatus::Ok
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Telemetry {
    pub rpm: f32,
    pub battery_voltage: f32,
    pub motor_current: f32,
    pub throttle: f32, // normalized 0–1
    pub controller_temp: f32,
    pub motor_temp: f32,
    pub direction: MotorDirection,
    pub feedback: MotorFeedback,
    pub brake: bool,
    pub foot_sw: bool,
    pub boost_sw: bool,
    pub kelly_faults: KellyFaults,
    pub mppt: [MpptData; 3],
}

// Same physical constants as the onboard dashboard.
pub const WHEEL_DIAMETER_M: f32 = 0.557;
pub const GEAR_RATIO: f32 = 1.0;
pub const METERS_PER_MILE: f32 = 1609.34;

impl Telemetry {
    pub fn speed_mph(&self) -> f32 {
        let wheel_rpm = self.rpm / GEAR_RATIO;
        wheel_rpm * WHEEL_DIAMETER_M * std::f32::consts::PI * 60.0 / METERS_PER_MILE
    }
    pub fn solar_in(&self) -> f32 {
        self.mppt.iter().map(|m| m.watts_in()).sum()
    }
    pub fn charge_out(&self) -> f32 {
        self.mppt.iter().map(|m| m.watts_out()).sum()
    }
    pub fn motor_draw(&self) -> f32 {
        self.battery_voltage * self.motor_current
    }
    pub fn net_pack(&self) -> f32 {
        self.charge_out() - self.motor_draw()
    }
    pub fn any_fault(&self) -> bool {
        self.kelly_faults.any() || self.mppt.iter().any(|m| m.fault || m.aux_fault)
    }
}

// ─── Frame → model ────────────────────────────────────────────────────────────

fn embedded_id(msg: &CanFrameMsg) -> Option<Id> {
    if msg.is_extended() {
        ExtendedId::new(msg.can_id()).map(Id::Extended)
    } else {
        u16::try_from(msg.can_id())
            .ok()
            .and_then(StandardId::new)
            .map(Id::Standard)
    }
}

/// Each MPPT unit has its own generated module (distinct CAN ids), so the
/// per-unit decode is stamped out per module.
macro_rules! try_mppt {
    ($module:ident, $slot:expr, $id:expr, $data:expr) => {
        if let Ok(m) = $module::Messages::from_can_message($id, $data) {
            use $module::Messages as M;
            match m {
                M::PowerInput(f) => {
                    $slot.input_voltage = f.input_voltage();
                    $slot.input_current = f.input_current();
                }
                M::PowerOutput(f) => {
                    $slot.output_voltage = f.output_voltage();
                    $slot.output_current = f.output_current();
                }
                M::Temperature(f) => $slot.mosfet_temp = f.mosfet_temperature(),
                M::Status(f) => {
                    $slot.fault = f.error_mosfet_overheat()
                        || f.error_low_arrow_power()
                        || f.error_hw_over_voltage()
                        || f.error_hw_over_current()
                        || f.error_battery_low()
                        || f.error_battery_full();
                    $slot.aux_fault = f.error12v_undervoltage();
                    $slot.limiting = f.limit_output_voltage_max()
                        || f.limit_mosfet_temperature()
                        || f.limit_local_mppt()
                        || f.limit_input_current_min()
                        || f.limit_input_current_max()
                        || f.limit_global_mppt()
                        || f.limit_duty_cycle_max()
                        || f.limit_dury_cycle_min();
                }
                _ => {}
            }
            return true;
        }
    };
}

/// Apply one radio-delivered CAN frame to the model.
/// Returns false if the id doesn't belong to any known message.
pub fn apply(t: &mut Telemetry, msg: &CanFrameMsg) -> bool {
    let Some(id) = embedded_id(msg) else {
        return false;
    };
    let data = msg.data.as_slice();

    if let Ok(m) = messages_kelly::Messages::from_can_message(id, data) {
        match m {
            messages_kelly::Messages::Message1(f) => {
                t.rpm = f.speed_rpm() as f32;
                t.motor_current = f.motor_current() as f32;
                t.battery_voltage = f.battery_voltage() as f32;
                t.kelly_faults = KellyFaults {
                    over_voltage: f.over_voltage(),
                    low_voltage: f.low_voltage(),
                    stall: f.stall(),
                    internal_volts: f.internal_volts_fault(),
                    controller_over_temp: f.over_temperature(),
                    motor_over_temp: f.motor_over_temperature(),
                    throttle_error: f.throttle_error(),
                    hall_throttle_open: f.hall_throttle_open(),
                    angle_sensor: f.angle_sensor_error(),
                    hall_galvanometer: f.hall_galvanometer_error(),
                    id_error: f.id_error(),
                };
            }
            messages_kelly::Messages::Message2(f) => {
                t.throttle = (f.throttle_signal() as f32 / 255.0).clamp(0.0, 1.0);
                t.controller_temp = f.controller_temperature() as f32;
                t.motor_temp = f.motor_temperature() as f32;
                t.direction = match f.command_status() {
                    messages_kelly::Message2CommandStatus::Backward => MotorDirection::Reverse,
                    messages_kelly::Message2CommandStatus::Forward => MotorDirection::Forward,
                    messages_kelly::Message2CommandStatus::Neutral => MotorDirection::Neutral,
                    _ => MotorDirection::Unknown,
                };
                t.feedback = match f.feedback_status() {
                    messages_kelly::Message2FeedbackStatus::Stationary => {
                        MotorFeedback::Stationary
                    }
                    messages_kelly::Message2FeedbackStatus::Forward => MotorFeedback::Forward,
                    messages_kelly::Message2FeedbackStatus::Backward => MotorFeedback::Backward,
                    _ => MotorFeedback::Unknown,
                };
                t.brake = f.brake_switch();
                t.foot_sw = f.foot_switch();
                t.boost_sw = f.boost_switch();
            }
        }
        return true;
    }

    try_mppt!(messages_mppt, t.mppt[0], id, data);
    try_mppt!(messages_mppt_2, t.mppt[1], id, data);
    try_mppt!(messages_mppt_3, t.mppt[2], id, data);

    false
}

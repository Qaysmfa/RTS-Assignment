use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::shared::TelemetryType;

pub const SENSOR_BUFFER_CAPACITY: usize = 64;
pub const TX_QUEUE_CAPACITY: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SensorKind {
    Thermal,
    Attitude,
    Power,
}

impl SensorKind {
    pub fn priority(self) -> u8 {
        match self {
            SensorKind::Thermal => 3,
            SensorKind::Attitude => 2,
            SensorKind::Power => 1,
        }
    }

    pub fn period(self) -> Duration {
        match self {
            SensorKind::Thermal => Duration::from_millis(25),
            SensorKind::Attitude => Duration::from_millis(20),
            SensorKind::Power => Duration::from_millis(50),
        }
    }

    pub fn telemetry_type(self) -> TelemetryType {
        match self {
            SensorKind::Thermal => TelemetryType::Thermal,
            SensorKind::Attitude => TelemetryType::Attitude,
            SensorKind::Power => TelemetryType::Power,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SensorSample {
    pub kind: SensorKind,
    pub value: f32,
    pub read_at: Instant,
    pub scheduled_at: Instant,
    pub inserted_at: Instant,
    pub corrupted: bool,
}

#[derive(Debug, Clone)]
pub struct DownlinkPacket {
    pub ttype: TelemetryType,
    pub payload: Vec<u8>,
    pub enqueued_at: Instant,
}

#[derive(Debug)]
pub struct OcsState {
    pub safe_mode: bool,
    pub degraded_mode: bool,
    pub active_fault: Option<String>,
    pub active_fault_since: Option<Instant>,
    pub mission_abort: bool,
    pub start: Instant,

    pub sensor_buffer: VecDeque<SensorSample>,
    pub tx_queue: VecDeque<DownlinkPacket>,

    pub packets_sent: u64,
    pub fault_packets: u64,
    pub dropped_packets: u64,
    pub jitter_injections: u64,
    pub commands_received: u64,
    pub ack_ok: u64,
    pub ack_rejected: u64,

    pub sensor_latency_us: Vec<u128>,
    pub sensor_drift_us: Vec<i128>,
    pub sensor_jitter_us: Vec<i128>,
    pub critical_jitter_violations: u64,
    pub thermal_missed_cycles: u32,
    pub safety_alerts: u64,

    pub schedule_drift_us: Vec<i128>,
    pub task_jitter_us: Vec<i128>,
    pub deadline_violations: u64,
    pub thermal_preemptions: u64,

    pub cpu_active_us: u128,
    pub cpu_idle_us: u128,

    pub tx_latency_us: Vec<u128>,
    pub buffer_fill_rates: Vec<f64>,
    pub tx_fill_rates: Vec<f64>,
    pub downlink_init_missed: u64,
    pub downlink_prep_deadline_missed: u64,

    pub faults_injected: u64,
    pub fault_responses: u64,
    pub fault_recovery_us: Vec<u128>,

    pub delayed_power_pending: bool,
    pub corrupt_next_sample: bool,
    pub fault_telemetry_pending: bool,

    pub command_response_latency_us: Vec<u128>,
}

impl OcsState {
    pub fn new() -> Self {
        Self {
            safe_mode: false,
            degraded_mode: false,
            active_fault: None,
            active_fault_since: None,
            mission_abort: false,
            start: Instant::now(),
            sensor_buffer: VecDeque::with_capacity(SENSOR_BUFFER_CAPACITY),
            tx_queue: VecDeque::with_capacity(TX_QUEUE_CAPACITY),
            packets_sent: 0,
            fault_packets: 0,
            dropped_packets: 0,
            jitter_injections: 0,
            commands_received: 0,
            ack_ok: 0,
            ack_rejected: 0,
            sensor_latency_us: Vec::new(),
            sensor_drift_us: Vec::new(),
            sensor_jitter_us: Vec::new(),
            critical_jitter_violations: 0,
            thermal_missed_cycles: 0,
            safety_alerts: 0,
            schedule_drift_us: Vec::new(),
            task_jitter_us: Vec::new(),
            deadline_violations: 0,
            thermal_preemptions: 0,
            cpu_active_us: 0,
            cpu_idle_us: 0,
            tx_latency_us: Vec::new(),
            buffer_fill_rates: Vec::new(),
            tx_fill_rates: Vec::new(),
            downlink_init_missed: 0,
            downlink_prep_deadline_missed: 0,
            faults_injected: 0,
            fault_responses: 0,
            fault_recovery_us: Vec::new(),
            delayed_power_pending: false,
            corrupt_next_sample: false,
            fault_telemetry_pending: false,
            command_response_latency_us: Vec::new(),
        }
    }

    pub fn uptime_secs(&self) -> u64 {
        self.start.elapsed().as_secs()
    }

    pub fn sensor_fill_ratio(&self) -> f64 {
        self.sensor_buffer.len() as f64 / SENSOR_BUFFER_CAPACITY as f64
    }

    pub fn tx_fill_ratio(&self) -> f64 {
        self.tx_queue.len() as f64 / TX_QUEUE_CAPACITY as f64
    }
}

pub type SharedState = Arc<Mutex<OcsState>>;

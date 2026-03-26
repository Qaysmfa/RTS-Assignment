use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use parking_lot::Mutex;

#[derive(Debug)]
pub struct TelemetryMetrics {
    pub last_seq: Option<u64>,
    pub consecutive_failures: u32,
    pub last_recv_instant: Option<Instant>,
    pub latency_samples: Vec<i128>,
    pub jitter_samples: Vec<i128>,
    pub drift_samples: Vec<i128>,
    pub scheduler_drift_samples: Vec<i128>,
    pub command_response_latency_ms: Vec<i128>,
    pub fault_recovery_ms: Vec<u128>,
    pub system_load_samples: Vec<i128>,
    pub active_fault: Option<String>,
    pub fault_detected_at: Option<Instant>,
    pub interlock_latency_us: Vec<u128>,
    pub total_packets: u64,
    pub missed_deadlines: u64,
    pub first_recv_instant: Option<Instant>,
    pub first_seq: Option<u64>,
    pub missing_packets_count: u32,
    pub urgent_command_success: u64,
    pub urgent_command_failures: u64,
    pub mission_abort_count: u32,
    pub telemetry_backlog_samples: Vec<u64>,
    pub cpu_usage_samples: Vec<f64>,
}

impl TelemetryMetrics {
    pub fn new() -> Self {
        Self {
            last_seq: None,
            consecutive_failures: 0,
            last_recv_instant: None,
            latency_samples: Vec::new(),
            jitter_samples: Vec::new(),
            drift_samples: Vec::new(),
            scheduler_drift_samples: Vec::new(),
            command_response_latency_ms: Vec::new(),
            fault_recovery_ms: Vec::new(),
            system_load_samples: Vec::new(),
            active_fault: None,
            fault_detected_at: None,
            interlock_latency_us: Vec::new(),
            total_packets: 0,
            missed_deadlines: 0,
            first_recv_instant: None,
            first_seq: None,
            missing_packets_count: 0,
            urgent_command_success: 0,
            urgent_command_failures: 0,
            mission_abort_count: 0,
            telemetry_backlog_samples: Vec::new(),
            cpu_usage_samples: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CommandEntry {
    pub id: u64,
    pub cmd_debug: String,
    pub deadline_instant: Instant,
    pub priority: u8,
    pub created_instant: Instant,
    pub rejection_reason: Option<String>,
}

#[derive(Clone)]
pub struct GcsState {
    pub metrics: Arc<Mutex<TelemetryMetrics>>,
    pub safe_mode: Arc<Mutex<bool>>,
    pub cmd_backlog: Arc<DashMap<u64, CommandEntry>>,
    pub uplink_jitter: Arc<Mutex<Vec<i128>>>,
    pub last_cmd_dispatch: Arc<Mutex<Option<Instant>>>,
    pub recovery_tx: Arc<tokio::sync::mpsc::Sender<()>>,
    pub telemetry_processing_start: Arc<Mutex<Option<Instant>>>,
    pub telemetry_backlog_count: Arc<Mutex<u64>>,
    pub cpu_busy_start: Arc<Mutex<Option<Instant>>>,
    pub cpu_total_active_ms: Arc<Mutex<u64>>,
}

impl GcsState {
    pub fn new(recovery_tx: Arc<tokio::sync::mpsc::Sender<()>>) -> Self {
        Self {
            recovery_tx,
            metrics: Arc::new(Mutex::new(TelemetryMetrics::new())),
            safe_mode: Arc::new(Mutex::new(false)),
            cmd_backlog: Arc::new(DashMap::new()),
            uplink_jitter: Arc::new(Mutex::new(Vec::new())),
            last_cmd_dispatch: Arc::new(Mutex::new(None)),
            telemetry_processing_start: Arc::new(Mutex::new(None)),
            telemetry_backlog_count: Arc::new(Mutex::new(0)),
            cpu_busy_start: Arc::new(Mutex::new(None)),
            cpu_total_active_ms: Arc::new(Mutex::new(0)),
        }
    }
}
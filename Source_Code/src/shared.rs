use serde::{Deserialize, Serialize};

pub const GCS_TELEMETRY_BIND: &str = "127.0.0.1:4000";
pub const OCS_CMD_BIND: &str = "127.0.0.1:5000";
pub const TELEMETRY_PERIOD_MS: u64 = 100;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum TelemetryType {
    Thermal,
    Attitude,
    Power,
    Fault(String),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TelemetryPacket {
    pub seq: u64,
    pub ttype: TelemetryType,
    pub sent_ts_millis: i64,
    pub payload: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Command {
    NoOp,
    ReRequestTelemetry { seq: u64 },
    Urgent { id: u64, body: String },
    ClearFault,
    QueryStatus,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum CommandAck {
    Ok { id: u64 },
    Rejected { id: u64, reason: String },
    StatusReport {
        safe_mode: bool,
        active_fault: Option<String>,
        uptime_secs: u64,
    },
}
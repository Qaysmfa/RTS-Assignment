use std::net::SocketAddr;
use std::time::{Duration, Instant};

use bincode;
use chrono::Utc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time;

use crate::gcs::state::{CommandEntry, GcsState};
use crate::shared::{Command, CommandAck, OCS_CMD_BIND};

const TCP_CONNECT_TIMEOUT_MS: u64 = 50;
pub const URGENT_DEADLINE_MS: u64 = 2;

pub async fn run(
    mut rx: mpsc::Receiver<(Command, Instant)>,
    state: GcsState,
) {
    while let Some((cmd, deadline)) = rx.recv().await {
        let dispatch_start = Instant::now();

        record_uplink_jitter(&state, dispatch_start);

        match send_and_receive_ack(&cmd).await {
            Ok(ack) => {
                let elapsed_ms = dispatch_start.elapsed().as_secs_f64() * 1000.0;
                state.metrics.lock().command_response_latency_ms.push(elapsed_ms as i128);
                
                if matches!(cmd, Command::Urgent { .. }) {
                    if elapsed_ms <= 2.0 {
                        state.metrics.lock().urgent_command_success += 1;
                    } else {
                        state.metrics.lock().urgent_command_failures += 1;
                    }
                }
                
                let was_accepted = matches!(ack, CommandAck::Ok { .. });
                check_dispatch_deadline(elapsed_ms, &cmd, &state, was_accepted);
                handle_ack(ack, &state);
            }
            Err(e) => {
                println!("[CMD-UPLINK][{}] WARN: dispatch failed ({}), parked in backlog.",
                    Utc::now(), e);
                park_in_backlog(cmd, deadline, &state, Some(format!("Dispatch error: {}", e)));
            }
        }

        state.cmd_backlog.retain(|_, e| e.created_instant.elapsed().as_secs() < 30);
    }
}

fn record_uplink_jitter(state: &GcsState, now: Instant) {
    let mut jitter = state.uplink_jitter.lock();
    let mut last   = state.last_cmd_dispatch.lock();
    if let Some(prev) = *last {
        jitter.push(now.duration_since(prev).as_millis() as i128);
    }
    *last = Some(now);
}

fn check_dispatch_deadline(
    elapsed_ms: f64,
    cmd: &Command,
    state: &GcsState,
    was_accepted: bool,
) {
    let missed = matches!(cmd, Command::Urgent { .. }) && elapsed_ms > 2.0;

    if missed && was_accepted {
        println!(
            "[CMD-UPLINK][{}] DEADLINE MISS: {:?} dispatched {:.2}ms  (limit=2ms)",
            Utc::now(), cmd, elapsed_ms
        );
        state.metrics.lock().missed_deadlines += 1;
    } else if missed && !was_accepted {
        println!(
            "[CMD-UPLINK][{}] NOTE: {:?} dispatch={:.2}ms but OCS rejected (not a GCS miss)",
            Utc::now(), cmd, elapsed_ms
        );
    } else if let Command::ReRequestTelemetry { seq } = cmd {
        println!(
            "[CMD-UPLINK][{}] SENT ReRequestTelemetry seq={}  dispatch={:.2}ms  deadline=OK",
            Utc::now(), seq, elapsed_ms
        );
    } else {
        println!(
            "[CMD-UPLINK][{}] SENT {:?}  dispatch={:.2}ms  deadline=OK",
            Utc::now(), cmd, elapsed_ms
        );
    }
}

fn handle_ack(ack: CommandAck, state: &GcsState) -> bool {
    match ack {
        CommandAck::Ok { id } => {
            println!("[CMD-UPLINK][{}] ACK Ok  id={}", Utc::now(), id);
            true
        }
        CommandAck::Rejected { id, reason } => {
            println!(
                "[CMD-UPLINK][{}] ACK Rejected id={} reason='{}'",
                Utc::now(), id, reason
            );
            false
        }
        CommandAck::StatusReport { safe_mode, ref active_fault, uptime_secs } => {
            println!(
                "[CMD-UPLINK][{}] StatusReport: safe_mode={} fault={:?} uptime={}s",
                Utc::now(), safe_mode, active_fault, uptime_secs
            );
            *state.safe_mode.lock() = safe_mode;
            state.metrics.lock().active_fault = active_fault.clone();
            true
        }
    }
}

fn park_in_backlog(cmd: Command, deadline: Instant, state: &GcsState, reason: Option<String>) {
    match cmd {
        Command::Urgent { id, .. } => {
            if state.cmd_backlog.contains_key(&id) {
                return;
            }
            state.cmd_backlog.insert(id, CommandEntry {
                id,
                cmd_debug: format!("{cmd:?}"),
                deadline_instant: deadline,
                priority: 1,
                created_instant: Instant::now(),
                rejection_reason: reason,
            });
        }
        _ => {
            println!("[CMD-UPLINK][{}] Non-urgent dispatch skipped (satellite unreachable).", Utc::now());
        }
    }
}

pub async fn send_and_receive_ack(cmd: &Command) -> anyhow::Result<CommandAck> {
    let addr: SocketAddr = OCS_CMD_BIND.parse()?;

    let mut stream = time::timeout(
        Duration::from_millis(TCP_CONNECT_TIMEOUT_MS),
        TcpStream::connect(addr),
    )
    .await
    .map_err(|_| anyhow::anyhow!("TCP connect timeout (>{}ms)", TCP_CONNECT_TIMEOUT_MS))?
    .map_err(|e| anyhow::anyhow!("TCP connect error: {e:?}"))?;

    let encoded = bincode::serialize(cmd)?;
    stream.write_all(&(encoded.len() as u32).to_be_bytes()).await?;
    stream.write_all(&encoded).await?;

    let mut ack_len_buf = [0u8; 4];
    stream.read_exact(&mut ack_len_buf).await?;
    let ack_len = u32::from_be_bytes(ack_len_buf) as usize;
    let mut ack_buf = vec![0u8; ack_len];
    stream.read_exact(&mut ack_buf).await?;

    Ok(bincode::deserialize::<CommandAck>(&ack_buf)?)
}

pub async fn retry_backlog_commands(state: &GcsState) {
    let backlog_ids: Vec<u64> = state.cmd_backlog.iter().map(|entry| *entry.key()).collect();
    
    for id in backlog_ids {
        if !*state.safe_mode.lock() {
            if let Some(entry) = state.cmd_backlog.get(&id) {
                println!("[CMD-UPLINK][{}] Retrying backlog command id={} (age={}ms, reason={:?})", 
                    Utc::now(), id, entry.created_instant.elapsed().as_millis(), entry.rejection_reason);
                
                let cmd_str = &entry.cmd_debug;
                if cmd_str.starts_with("Urgent") {
                    let urgent_cmd = Command::Urgent { id, body: format!("attitude-correct-{}", id) };
                    
                    match send_and_receive_ack(&urgent_cmd).await {
                        Ok(ack) => {
                            if matches!(ack, CommandAck::Ok { .. }) {
                                println!("[CMD-UPLINK][{}] Backlog command {} succeeded!", Utc::now(), id);
                                state.cmd_backlog.remove(&id);
                            } else {
                                println!("[CMD-UPLINK][{}] Backlog command {} rejected by OCS", Utc::now(), id);
                            }
                        }
                        Err(e) => {
                            println!("[CMD-UPLINK][{}] Backlog command {} retry failed: {}", Utc::now(), id, e);
                        }
                    }
                }
            }
        }
    }
}
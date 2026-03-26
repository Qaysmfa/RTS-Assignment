use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::shared::{GCS_TELEMETRY_BIND, OCS_CMD_BIND};

pub mod command;
pub mod downlink;
pub mod fault;
pub mod scheduler;
pub mod sensor;
pub mod state;
pub mod telemetry;

use state::{OcsState, SharedState};

pub async fn run() -> anyhow::Result<()> {
    print_banner();

    let state: SharedState = Arc::new(tokio::sync::Mutex::new(OcsState::new()));
    let token = CancellationToken::new();

    let command_handle = {
        let s = state.clone();
        let t = token.clone();
        tokio::spawn(async move {
            if let Err(e) = command::command_listener(s, t).await {
                eprintln!("[SAT-CMD] listener error: {e:?}");
            }
        })
    };

    let telemetry_handle = {
        let s = state.clone();
        let t = token.clone();
        tokio::spawn(async move {
            if let Err(e) = telemetry::telemetry_sender(s, t).await {
                eprintln!("[SAT-TEL] sender error: {e:?}");
            }
        })
    };

    let sensor_handle = {
        let s = state.clone();
        let t = token.clone();
        tokio::spawn(async move {
            sensor::run(s, t).await;
        })
    };

    let scheduler_handle = {
        let s = state.clone();
        let t = token.clone();
        tokio::spawn(async move {
            scheduler::run(s, t).await;
        })
    };

    let downlink_handle = {
        let s = state.clone();
        let t = token.clone();
        tokio::spawn(async move {
            downlink::run(s, t).await;
        })
    };

    let fault_handle = {
        let s = state.clone();
        let t = token.clone();
        tokio::spawn(async move {
            fault::run(s, t).await;
        })
    };

    tokio::signal::ctrl_c().await?;
    println!("\n[SAT] Ctrl-C received – shutting down...");
    token.cancel();

    let _ = command_handle.await;
    let _ = telemetry_handle.await;
    let _ = sensor_handle.await;
    let _ = scheduler_handle.await;
    let _ = downlink_handle.await;
    let _ = fault_handle.await;

    print_summary(&state).await;
    println!("[SAT] Goodbye.");

    Ok(())
}

fn print_banner() {
    println!("╔══════════════════════════════════════════════╗");
    println!("║  OCS – Satellite Onboard Control (Student A) ║");
    println!("╠══════════════════════════════════════════════╣");
    println!("║  Sends telemetry  → {}        ║", GCS_TELEMETRY_BIND);
    println!("║  Listens commands ← {}        ║", OCS_CMD_BIND);
    println!("╚══════════════════════════════════════════════╝");
    println!("Run this together with 'cargo run --bin gcs'.");
    println!("Press Ctrl+C to stop both gracefully.\n");
}

async fn print_summary(state: &SharedState) {
    let snapshot = state.lock().await;
    let (sensor_lat_min, sensor_lat_avg, sensor_lat_max) = stats_u128(&snapshot.sensor_latency_us);
    let (sensor_drift_min, sensor_drift_avg, sensor_drift_max) = stats_i128(&snapshot.sensor_drift_us);
    let (sensor_jit_min, sensor_jit_avg, sensor_jit_max) = stats_i128(&snapshot.sensor_jitter_us);
    let (sched_drift_min, sched_drift_avg, sched_drift_max) = stats_i128(&snapshot.schedule_drift_us);
    let (task_jit_min, task_jit_avg, task_jit_max) = stats_i128(&snapshot.task_jitter_us);
    let (tx_lat_min, tx_lat_avg, tx_lat_max) = stats_u128(&snapshot.tx_latency_us);
    let (fault_rec_min, fault_rec_avg, fault_rec_max) = stats_u128(&snapshot.fault_recovery_us);
    let (cmd_lat_min, cmd_lat_avg, cmd_lat_max) = stats_u128(&snapshot.command_response_latency_us);
    let cpu_total = snapshot.cpu_active_us + snapshot.cpu_idle_us;
    let cpu_util = if cpu_total == 0 {
        0.0
    } else {
        (snapshot.cpu_active_us as f64 * 100.0) / cpu_total as f64
    };

    println!("+----------------------------------------------------------------+");
    println!("| SATELLITE FINAL SUMMARY                                        |");
    println!("+----------------------------------------------------------------+");
    println!("uptime_secs                    : {}", snapshot.uptime_secs());
    println!("packets_sent                   : {}", snapshot.packets_sent);
    println!("fault_packets                  : {}", snapshot.fault_packets);
    println!("simulated_drops                : {}", snapshot.dropped_packets);
    println!("jitter_injections              : {}", snapshot.jitter_injections);
    println!("commands_received              : {}", snapshot.commands_received);
    println!("ack_ok                         : {}", snapshot.ack_ok);
    println!("ack_rejected                   : {}", snapshot.ack_rejected);
    println!("safe_mode                      : {}", snapshot.safe_mode);
    println!("degraded_mode                  : {}", snapshot.degraded_mode);
    println!("mission_abort                  : {}", snapshot.mission_abort);
    println!("active_fault                   : {:?}", snapshot.active_fault);
    println!("safety_alerts                  : {}", snapshot.safety_alerts);
    println!("thermal_missed_cycles          : {}", snapshot.thermal_missed_cycles);
    println!("critical_jitter_violations     : {}", snapshot.critical_jitter_violations);
    println!("deadline_violations            : {}", snapshot.deadline_violations);
    println!("thermal_preemptions            : {}", snapshot.thermal_preemptions);
    println!("downlink_init_missed           : {}", snapshot.downlink_init_missed);
    println!("downlink_prep_missed           : {}", snapshot.downlink_prep_deadline_missed);
    println!("faults_injected                : {}", snapshot.faults_injected);
    println!("fault_responses                : {}", snapshot.fault_responses);
    println!("cpu_utilization_percent        : {:.2}", cpu_util);
    println!("sensor_latency_us min/avg/max  : {}/{}/{}", sensor_lat_min, sensor_lat_avg, sensor_lat_max);
    println!("sensor_drift_us min/avg/max    : {}/{}/{}", sensor_drift_min, sensor_drift_avg, sensor_drift_max);
    println!("sensor_jitter_us min/avg/max   : {}/{}/{}", sensor_jit_min, sensor_jit_avg, sensor_jit_max);
    println!("sched_drift_us min/avg/max     : {}/{}/{}", sched_drift_min, sched_drift_avg, sched_drift_max);
    println!("task_jitter_us min/avg/max     : {}/{}/{}", task_jit_min, task_jit_avg, task_jit_max);
    println!("tx_latency_us min/avg/max      : {}/{}/{}", tx_lat_min, tx_lat_avg, tx_lat_max);
    println!("fault_recovery_us min/avg/max  : {}/{}/{}", fault_rec_min, fault_rec_avg, fault_rec_max);
    println!("cmd_resp_us min/avg/max        : {}/{}/{}", cmd_lat_min, cmd_lat_avg, cmd_lat_max);
    println!("+----------------------------------------------------------------+");
}

fn stats_u128(v: &[u128]) -> (u128, u128, u128) {
    if v.is_empty() {
        return (0, 0, 0);
    }
    let min = *v.iter().min().unwrap_or(&0);
    let max = *v.iter().max().unwrap_or(&0);
    let avg = v.iter().sum::<u128>() / v.len() as u128;
    (min, avg, max)
}

fn stats_i128(v: &[i128]) -> (i128, i128, i128) {
    if v.is_empty() {
        return (0, 0, 0);
    }
    let min = *v.iter().min().unwrap_or(&0);
    let max = *v.iter().max().unwrap_or(&0);
    let avg = v.iter().sum::<i128>() / v.len() as i128;
    (min, avg, max)
}

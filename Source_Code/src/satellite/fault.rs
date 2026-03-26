use std::time::{Duration, Instant};

use chrono::Utc;
use tokio::time;
use tokio_util::sync::CancellationToken;

use crate::satellite::state::SharedState;

pub async fn run(state: SharedState, token: CancellationToken) {
    let mut inject_interval = time::interval_at(
        time::Instant::now() + Duration::from_secs(60),
        Duration::from_secs(60),
    );
    let mut delayed_fault = true;

    loop {
        tokio::select! {
            _ = token.cancelled() => {
                println!("[FAULT] shutdown.");
                break;
            }
            _ = inject_interval.tick() => {
                inject_fault(&state, delayed_fault).await;
                delayed_fault = !delayed_fault;
            }
            _ = time::sleep(Duration::from_millis(20)) => {
                monitor_fault_recovery(&state).await;
            }
        }
    }
}

async fn inject_fault(state: &SharedState, delayed_fault: bool) {
    let mut ocs = state.lock().await;
    ocs.faults_injected += 1;
    ocs.safe_mode = true;
    ocs.active_fault_since = Some(Instant::now());
    ocs.fault_telemetry_pending = true;

    if delayed_fault {
        ocs.active_fault = Some("fault_injected:delayed_power_data".to_string());
        ocs.delayed_power_pending = true;
        println!(
            "[FAULT][{}] injected delayed power data fault",
            Utc::now()
        );
    } else {
        ocs.active_fault = Some("fault_injected:corrupted_sensor_data".to_string());
        ocs.corrupt_next_sample = true;
        println!(
            "[FAULT][{}] injected corrupted sensor data fault",
            Utc::now()
        );
    }
}

async fn monitor_fault_recovery(state: &SharedState) {
    let (fault, since) = {
        let ocs = state.lock().await;
        (ocs.active_fault.clone(), ocs.active_fault_since)
    };

    let (fault_desc, started) = match (fault, since) {
        (Some(f), Some(s)) => (f, s),
        _ => return,
    };

    let elapsed = started.elapsed();

    if elapsed > Duration::from_millis(200) {
        let mut ocs = state.lock().await;
        if !ocs.mission_abort {
            ocs.mission_abort = true;
            ocs.safe_mode = true;
            println!(
                "[FAULT][{}] MISSION ABORT: recovery {}ms (>200ms) fault='{}'",
                Utc::now(),
                elapsed.as_millis(),
                fault_desc
            );
        }
        return;
    }

    if elapsed > Duration::from_millis(120) {
        let mut ocs = state.lock().await;
        if ocs.active_fault.is_some() {
            ocs.fault_responses += 1;
            ocs.fault_recovery_us.push(elapsed.as_micros());
            ocs.active_fault = None;
            ocs.active_fault_since = None;
            ocs.fault_telemetry_pending = false;
            ocs.safe_mode = false;
            println!(
                "[FAULT][{}] recovered in {}ms fault='{}'",
                Utc::now(),
                elapsed.as_millis(),
                fault_desc
            );
        }
    }
}

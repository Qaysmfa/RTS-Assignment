use std::time::Duration;

use chrono::Utc;
use tokio::time;

use crate::gcs::state::GcsState;

const FAULT_RESPONSE_DEADLINE_MS: u128 = 100;
const WATCHDOG_POLL_MS: u64 = 20;
const AUTO_RECOVERY_MS: u128 = 180;
const MISSION_ABORT_THRESHOLD_MS: u128 = 200;

pub async fn run(state: GcsState) {
    let mut interval = time::interval(Duration::from_millis(WATCHDOG_POLL_MS));
    let mut alerted = false;
    let mut abort_logged = false;

    loop {
        interval.tick().await;

        let (fault_desc, detected_at) = {
            let met = state.metrics.lock();
            (met.active_fault.clone(), met.fault_detected_at)
        };

        if fault_desc.is_none() {
            alerted = false;
            abort_logged = false;
            continue;
        }

        let (desc, at) = match (fault_desc, detected_at) {
            (Some(d), Some(t)) => (d, t),
            _ => continue,
        };

        let elapsed_ms = at.elapsed().as_millis();

        if !alerted && elapsed_ms > FAULT_RESPONSE_DEADLINE_MS {
            trigger_critical_alert(&desc, elapsed_ms);
            alerted = true;
        }

        if elapsed_ms > AUTO_RECOVERY_MS {
            let fault_still_active = {
                let met = state.metrics.lock();
                met.active_fault.is_some()
            };
            
            if fault_still_active {
                let recovery_success = perform_auto_recovery(&desc, elapsed_ms, &state);
                
                if !recovery_success && !abort_logged {
                    state.metrics.lock().mission_abort_count += 1;
                    println!(
                        "[FAULT-WDG][{}] *** MISSION ABORT ***\n\
                         \t  Fault '{}' recovery took {}ms\n\
                         \t  Exceeds maximum allowed {}ms.\n\
                         \t  Simulating mission termination.",
                        Utc::now(),
                        desc,
                        elapsed_ms,
                        MISSION_ABORT_THRESHOLD_MS
                    );
                    abort_logged = true;
                }
                alerted = false;
            }
        }
    }
}

fn trigger_critical_alert(desc: &str, elapsed_ms: u128) {
    println!(
        "[FAULT-WDG][{}] *** CRITICAL GROUND ALERT ***\n\
         \t  Fault     : '{}'\n\
         \t  Unresolved: {}ms  (SLA = {}ms)\n\
         \t  Action    : Escalate to mission controller.",
        Utc::now(),
        desc,
        elapsed_ms,
        FAULT_RESPONSE_DEADLINE_MS
    );
}

fn perform_auto_recovery(desc: &str, elapsed_ms: u128, state: &GcsState) -> bool {
    let success = elapsed_ms <= MISSION_ABORT_THRESHOLD_MS;
    
    if success {
        println!(
            "[FAULT-WDG][{}] Auto-recovery: clearing fault '{}' after {}ms.\n\
             \t  Disengaging interlock – safe_mode = false.",
            Utc::now(), desc, elapsed_ms
        );
    } else {
        println!(
            "[FAULT-WDG][{}] Recovery FAILED: fault '{}' persisted for {}ms (exceeds {}ms).",
            Utc::now(), desc, elapsed_ms, MISSION_ABORT_THRESHOLD_MS
        );
    }
    
    *state.safe_mode.lock() = false;
    {
        let mut met = state.metrics.lock();
        met.active_fault = None;
        met.fault_detected_at = None;
        met.fault_recovery_ms.push(elapsed_ms);
    }
    
    if let Err(e) = state.recovery_tx.try_send(()) {
        eprintln!("[FAULT-WDG] Could not queue ClearFault: {e:?}");
    }
    
    success
}
use std::sync::Arc;
use std::time::Instant;
use std::time::Duration;
use chrono::Utc;

use tokio::sync::mpsc;
use tokio::time;
use tokio_util::sync::CancellationToken;

use gcs::gcs::{fault, monitor, scheduler, state::GcsState, telemetry, uplink};
use gcs::shared::{Command, GCS_TELEMETRY_BIND, OCS_CMD_BIND};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("╔══════════════════════════════════════════════╗");
    println!("║  GCS – Ground Control Station  (Student B)   ║");
    println!("╠══════════════════════════════════════════════╣");
    println!("║  Telemetry UDP : {}              ║", GCS_TELEMETRY_BIND);
    println!("║  OCS TCP cmd   : {}              ║", OCS_CMD_BIND);
    println!("╚══════════════════════════════════════════════╝");

    let (recovery_tx, mut recovery_rx) = mpsc::channel::<()>(8);
    let state = GcsState::new(Arc::new(recovery_tx));
    let cpu_active_start = state.cpu_busy_start.clone();
    let cpu_total = state.cpu_total_active_ms.clone();
    let token = CancellationToken::new();

    let (cmd_tx, cmd_rx) = mpsc::channel::<(Command, Instant)>(256);

    {
        let s = state.clone(); let t = token.clone(); let tx = cmd_tx.clone();
        tokio::spawn(async move {
            tokio::select! {
                res = telemetry::run(s, tx) => {
                    if let Err(e) = res { eprintln!("[TELEMETRY] error: {e:?}"); }
                }
                _ = t.cancelled() => println!("[TELEMETRY] shutdown."),
            }
        });
    }

    {
        let s = state.clone(); let t = token.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = uplink::run(cmd_rx, s) => {}
                _ = t.cancelled() => println!("[CMD-UPLINK] shutdown."),
            }
        });
    }

    {
        let s = state.clone(); let t = token.clone(); let tx = cmd_tx.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = scheduler::run(s, tx) => {}
                _ = t.cancelled() => println!("[CMD-SCHED] shutdown."),
            }
        });
    }

    {
        let s = state.clone(); let t = token.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = fault::run(s) => {}
                _ = t.cancelled() => println!("[FAULT-WDG] shutdown."),
            }
        });
    }

    {
        let s = state.clone(); let t = token.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = monitor::run(s) => {}
                _ = t.cancelled() => println!("[MONITOR] shutdown."),
            }
        });
    }

    {
        let t = token.clone();
        let cpu_start = cpu_active_start.clone();
        let cpu_total_ms = cpu_total.clone();
        tokio::spawn(async move {
            let mut last_active = Instant::now();
            *cpu_start.lock() = Some(last_active);
            
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(10)) => {
                        let now = Instant::now();
                        let active_ms = now.duration_since(last_active).as_millis() as u64;
                        *cpu_total_ms.lock() += active_ms;
                        last_active = now;
                        *cpu_start.lock() = Some(now);
                    }
                    _ = t.cancelled() => break,
                }
            }
        });
    }

    {
        let t = token.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(_) = recovery_rx.recv() => {
                        if let Err(e) = uplink::send_and_receive_ack(&Command::ClearFault).await {
                            eprintln!("[RECOVERY] ClearFault send failed: {e:?}");
                        } else {
                            println!("[RECOVERY][{}] ClearFault sent to OCS.", Utc::now());
                        }
                    }
                    _ = t.cancelled() => break,
                }
            }
        });
    }

    {
        let s = state.clone();
        let t = token.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let safe_mode = *s.safe_mode.lock();
                        if !safe_mode {
                            uplink::retry_backlog_commands(&s).await;
                        }
                    }
                    _ = t.cancelled() => break,
                }
            }
        });
    }

    tokio::signal::ctrl_c().await?;
    println!("\n[GCS] Ctrl-C received – shutting down...");
    token.cancel();
    time::sleep(std::time::Duration::from_millis(500)).await;
    
    println!("\n╔══════════════════════════════════════════════════╗");
    println!("║  GCS FINAL PERFORMANCE SUMMARY                   ║");
    println!("╚══════════════════════════════════════════════════╝");
    
    let met = state.metrics.lock();
    let total_packets = met.total_packets;
    let missed = met.missed_deadlines;
    let miss_rate = if total_packets > 0 {
        (missed as f64 / total_packets as f64) * 100.0
    } else {
        0.0
    };
    
    let avg_fault_recovery = if met.fault_recovery_ms.is_empty() {
        0
    } else {
        met.fault_recovery_ms.iter().sum::<u128>() / met.fault_recovery_ms.len() as u128
    };
    
    let avg_interlock = if met.interlock_latency_us.is_empty() {
        0
    } else {
        met.interlock_latency_us.iter().sum::<u128>() / met.interlock_latency_us.len() as u128
    };
    
    let avg_cmd_response = if met.command_response_latency_ms.is_empty() {
        0
    } else {
        met.command_response_latency_ms.iter().sum::<i128>() / met.command_response_latency_ms.len() as i128
    };
    
    println!("Total packets received: {}", total_packets);
    println!("Total missed deadlines: {}", missed);
    println!("Missed deadline rate: {:.2}%", miss_rate);
    println!("Average fault recovery time: {}ms", avg_fault_recovery);
    println!("Average interlock latency: {}µs", avg_interlock);
    println!("Average command response latency: {}ms", avg_cmd_response);
    println!("Urgent command successes: {}", met.urgent_command_success);
    println!("Urgent command deadline misses: {}", met.urgent_command_failures);
    println!("Mission abort count: {}", met.mission_abort_count);
    
    let backlog_size = state.cmd_backlog.len();
    println!("Remaining backlog entries: {}", backlog_size);
    
    if backlog_size > 0 {
        println!("\nPending backlog commands:");
        for entry in state.cmd_backlog.iter() {
            let e = entry.value();
            println!("  - id={} age={}ms {}", e.id, e.created_instant.elapsed().as_millis(), e.cmd_debug);
        }
    }
    
    println!("\n[GCS] Goodbye.");
    Ok(())
}
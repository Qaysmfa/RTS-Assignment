use std::time::{Duration, Instant};

use chrono::Utc;
use tokio::time;

use crate::gcs::state::GcsState;

const MONITOR_INTERVAL_SECS: u64 = 5;

pub async fn run(state: GcsState) {
    let mut interval = time::interval(Duration::from_secs(MONITOR_INTERVAL_SECS));
    let mut last_cpu_sample = Instant::now();

    loop {
        interval.tick().await;
        
        // Measure CPU usage (approximation: time spent in active processing)
        measure_cpu_usage(&state, last_cpu_sample);
        last_cpu_sample = Instant::now();
        
        print_report(&state);
    }
}

fn measure_cpu_usage(state: &GcsState, since: Instant) {
    let elapsed_ms = since.elapsed().as_millis() as u64;
    let active_ms = *state.cpu_total_active_ms.lock();
    
    if elapsed_ms > 0 {
        let cpu_percent = (active_ms as f64 / elapsed_ms as f64) * 100.0;
        state.metrics.lock().cpu_usage_samples.push(cpu_percent);
        
        // Reset for next interval
        *state.cpu_total_active_ms.lock() = 0;
        
        println!("[MONITOR] CPU Usage: {:.1}% active over last {}ms", cpu_percent, elapsed_ms);
    }
}

fn stats(v: &[i128]) -> (i128, i128, i128) {
    if v.is_empty() {
        return (0, 0, 0);
    }
    let avg = v.iter().sum::<i128>() / v.len() as i128;
    (*v.iter().min().unwrap(), avg, *v.iter().max().unwrap())
}

fn print_report(state: &GcsState) {
    let backlog_len = state.cmd_backlog.len() as i128;
    {
        let mut met = state.metrics.lock();
        met.system_load_samples.push(backlog_len);
    }

    let met = state.metrics.lock();
    let uplink = state.uplink_jitter.lock();

    let (lat_min, lat_avg, lat_max) = stats(&met.latency_samples);
    let (jit_min, jit_avg, jit_max) = stats(&met.jitter_samples);
    let (dft_min, dft_avg, dft_max) = stats(&met.drift_samples);
    let (upl_min, upl_avg, upl_max) = stats(&uplink);
    let (sch_min, sch_avg, sch_max) = stats(&met.scheduler_drift_samples);
    let (cmd_min, cmd_avg, cmd_max) = stats(&met.command_response_latency_ms);
    let (load_min, load_avg, load_max) = stats(&met.system_load_samples);

    let sch_samples = met.scheduler_drift_samples.len();
    if sch_samples == 0 {
        println!("  [WARNING] No scheduler drift samples collected!");
    }

    let il_count = met.interlock_latency_us.len();
    let il_avg_us = if il_count > 0 {
        met.interlock_latency_us.iter().sum::<u128>() / il_count as u128
    } else {
        0
    };

    let telemetry_backlog_avg = if met.telemetry_backlog_samples.is_empty() {
        0.0
    } else {
        met.telemetry_backlog_samples.iter().sum::<u64>() as f64 / met.telemetry_backlog_samples.len() as f64
    };

    let cpu_avg = if met.cpu_usage_samples.is_empty() {
        0.0
    } else {
        met.cpu_usage_samples.iter().sum::<f64>() / met.cpu_usage_samples.len() as f64
    };

    let safe_mode = *state.safe_mode.lock();
    let backlog_len = state.cmd_backlog.len();
    let fault_str = met.active_fault.as_deref().unwrap_or("none");
    let fault_rec_avg = if met.fault_recovery_ms.is_empty() {
        0
    } else {
        met.fault_recovery_ms.iter().sum::<u128>() / met.fault_recovery_ms.len() as u128
    };

    println!("\n╔══════════════════════════════════════════════════════════════════╗");
    println!("║  GCS SYSTEM PERFORMANCE MONITOR  [{ts}]                      ║",
             ts = Utc::now().format("%H:%M:%S"));
    println!("╠══════════════════════════════════════════════════════════════════╣");
    println!("║  Packets received : {tot:<6}                                       ║",
             tot = met.total_packets);
    println!("║  Missed deadlines : {miss:<6}                                       ║",
             miss = met.missed_deadlines);
    println!("║  Missing packets  : {miss_pkt:<6}                                       ║",
             miss_pkt = met.missing_packets_count);
    println!("║  Consec failures  : {cf:<6}                                       ║",
             cf = met.consecutive_failures);
    println!("║  Safe mode        : {sm:<6}                                       ║",
             sm = safe_mode);
    println!("║  CPU Usage (avg)  : {cpu:>5.1}%                                       ║",
             cpu = cpu_avg);
    println!("║  System load(avg) : {sl:<6} backlog units                         ║",
             sl = load_avg);
    println!("║  Active fault     : {af:<43}  ║",
             af = fault_str);
    println!("╠═══════════════════════════════╦══════╦══════╦══════╦═════════════╣");
    println!("║  Metric                       ║  min ║  avg ║  max ║  samples    ║");
    println!("╠═══════════════════════════════╬══════╬══════╬══════╬═════════════╣");
    println!("║  Telemetry latency      (ms)  ║{:>6}║{:>6}║{:>6}║ {:>12}║",
             lat_min, lat_avg, lat_max, met.latency_samples.len());
    println!("║  Telemetry jitter       (ms)  ║{:>6}║{:>6}║{:>6}║ {:>12}║",
             jit_min, jit_avg, jit_max, met.jitter_samples.len());
    println!("║  Scheduling drift       (ms)  ║{:>6}║{:>6}║{:>6}║ {:>12}║",
             dft_min, dft_avg, dft_max, met.drift_samples.len());
    println!("║  Uplink jitter          (ms)  ║{:>6}║{:>6}║{:>6}║ {:>12}║",
             upl_min, upl_avg, upl_max, uplink.len());
    println!("║  Scheduler drift        (ms)  ║{:>6}║{:>6}║{:>6}║ {:>12}║",
             sch_min, sch_avg, sch_max, met.scheduler_drift_samples.len());
    println!("║  Cmd response latency   (ms)  ║{:>6}║{:>6}║{:>6}║ {:>12}║",
             cmd_min, cmd_avg, cmd_max, met.command_response_latency_ms.len());
    println!("║  System load(backlog)   (n)   ║{:>6}║{:>6}║{:>6}║ {:>12}║",
             load_min, load_avg, load_max, met.system_load_samples.len());
    println!("╠═══════════════════════════════╩══════╩══════╩══════╩═════════════╣");
    println!("║  Interlock latency: avg={il:<6}µs    |      samples={n:<4}          ║",
             il = il_avg_us, n = il_count);
    println!("║  Telemetry backlog: avg={tb:<6.0} packets waiting                   ║",
             tb = telemetry_backlog_avg);
    println!("║  Command backlog: {bl:<4}entries                                    ║",
             bl = backlog_len);
    println!("║  Fault recovery: avg={fr:<4}ms         |      samples={frn:<4}          ║",
             fr = fault_rec_avg, frn = met.fault_recovery_ms.len());
    println!("║  Mission aborts: {abort:<4}                                            ║",
             abort = met.mission_abort_count);
    println!("╚══════════════════════════════════════════════════════════════════╝");

    for entry in state.cmd_backlog.iter() {
        let e = entry.value();
        let overdue_ms = if Instant::now() > e.deadline_instant {
            Instant::now()
                .duration_since(e.deadline_instant)
                .as_millis()
        } else {
            0
        };
        println!(
            "  [BACKLOG] id={} pri={} overdue={}ms age={}ms reason={:?}  {}",
            e.id,
            e.priority,
            overdue_ms,
            e.created_instant.elapsed().as_millis(),
            e.rejection_reason,
            e.cmd_debug
        );
    }
}
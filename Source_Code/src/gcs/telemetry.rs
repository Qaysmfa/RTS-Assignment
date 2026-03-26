use std::time::{Duration, Instant};

use bincode;
use chrono::Utc;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time;

use crate::gcs::state::GcsState;
use crate::shared::{Command, TelemetryPacket, TelemetryType, GCS_TELEMETRY_BIND, TELEMETRY_PERIOD_MS};

const DECODE_DEADLINE_MS: f64 = 3.0;
const URGENT_DEADLINE_MS: u64 = 2;

pub async fn run(
    state: GcsState,
    cmd_tx: mpsc::Sender<(Command, Instant)>,
) -> anyhow::Result<()> {
    let socket = UdpSocket::bind(GCS_TELEMETRY_BIND).await?;
    println!("[TELEMETRY] Listening on {}", GCS_TELEMETRY_BIND);

    let mut buf = vec![0u8; 4096];
    let expected_period_ms = TELEMETRY_PERIOD_MS as i128;

    loop {
        // Start tracking processing time for backlog measurement
        *state.telemetry_processing_start.lock() = Some(Instant::now());
        
        let recv_result = time::timeout(
            Duration::from_millis(TELEMETRY_PERIOD_MS * 2),
            socket.recv_from(&mut buf),
        )
        .await;

        // Record backlog: time packet waited before processing
        if let Some(start) = *state.telemetry_processing_start.lock() {
            let backlog_us = start.elapsed().as_micros();
            if backlog_us > 0 {
                let mut backlog_count = state.telemetry_backlog_count.lock();
                *backlog_count += 1;
                if *backlog_count % 100 == 0 {
                    state.metrics.lock().telemetry_backlog_samples.push(*backlog_count);
                    println!("[TELEMETRY][{}] Telemetry backlog: {} packets waiting",
                        Utc::now(), backlog_count);
                }
            }
        }

        match recv_result {
            Err(_) => {
                handle_timeout(&state, &cmd_tx).await;
                // Reset processing start on timeout
                *state.telemetry_processing_start.lock() = None;
            }
            Ok(Err(e)) => {
                eprintln!("[TELEMETRY] recv error: {e:?}");
                *state.telemetry_processing_start.lock() = None;
            }
            Ok(Ok((len, peer))) => {
                let recv_instant = Instant::now();
                let recv_wall_ms = Utc::now().timestamp_millis();

                let pkt = match decode_packet(&buf[..len], &state) {
                    None => {
                        *state.telemetry_processing_start.lock() = None;
                        continue;
                    }
                    Some(p) => p,
                };

                // Decrement backlog counter when packet is processed
                {
                    let mut backlog_count = state.telemetry_backlog_count.lock();
                    if *backlog_count > 0 {
                        *backlog_count -= 1;
                    }
                }
                *state.telemetry_processing_start.lock() = None;

                let decode_ms = recv_instant.elapsed().as_secs_f64() * 1000.0;
                check_decode_deadline(decode_ms, pkt.seq, &state);

                let latency_ms = recv_wall_ms - pkt.sent_ts_millis;

                update_metrics(
                    &pkt,
                    recv_instant,
                    latency_ms,
                    expected_period_ms,
                    &state,
                    &cmd_tx,
                    peer.to_string(),
                );

                handle_fault_packet(&pkt, &state);

                log_packet(&pkt, latency_ms, decode_ms);
            }
        }
    }
}

async fn handle_timeout(state: &GcsState, cmd_tx: &mpsc::Sender<(Command, Instant)>) {
    println!(
        "[TELEMETRY][{}] WARN: no packet in {}ms – possible link drop.",
        Utc::now(),
        TELEMETRY_PERIOD_MS * 2
    );

    let expected_seq = {
        let met = state.metrics.lock();
        met.last_seq.map(|s| s + 1).unwrap_or(0)
    };
    let deadline = Instant::now() + Duration::from_millis(URGENT_DEADLINE_MS);
    match cmd_tx.try_send((Command::ReRequestTelemetry { seq: expected_seq }, deadline)) {
        Ok(_) => println!(
            "[TELEMETRY][{}] Re-request sent for missing seq={}",
            Utc::now(),
            expected_seq
        ),
        Err(e) => eprintln!(
            "[TELEMETRY][{}] Re-request could not be queued for seq={}: {e:?}",
            Utc::now(),
            expected_seq
        ),
    }

    let mut met = state.metrics.lock();
    met.consecutive_failures += 1;
    if met.consecutive_failures >= 3 {
        println!(
            "[TELEMETRY][{}] *** LOSS-OF-CONTACT: {} consecutive failures ***",
            Utc::now(),
            met.consecutive_failures
        );
    }
}

fn decode_packet(data: &[u8], _state: &GcsState) -> Option<TelemetryPacket> {
    match bincode::deserialize::<TelemetryPacket>(data) {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("[TELEMETRY] Deserialise error (corrupt packet?): {e:?}");
            None
        }
    }
}

fn check_decode_deadline(decode_ms: f64, seq: u64, state: &GcsState) {
    if decode_ms > DECODE_DEADLINE_MS {
        println!(
            "[TELEMETRY][{}] DECODE DEADLINE MISS: {:.3}ms > {:.0}ms  seq={}",
            Utc::now(),
            decode_ms,
            DECODE_DEADLINE_MS,
            seq
        );
        state.metrics.lock().missed_deadlines += 1;
    }
}

fn update_metrics(
    pkt: &TelemetryPacket,
    recv_instant: Instant,
    latency_ms: i64,
    expected_period_ms: i128,
    state: &GcsState,
    cmd_tx: &mpsc::Sender<(Command, Instant)>,
    peer: String,
) {
    let mut met = state.metrics.lock();
    met.total_packets += 1;

    let expected_seq = met.last_seq.map(|s| s + 1).unwrap_or(pkt.seq);
    if pkt.seq != expected_seq && met.last_seq.is_some() {
        let gap = pkt.seq as i64 - expected_seq as i64;
        if gap > 0 {
            println!(
                "[TELEMETRY][{}] GAP: expected={} got={} gap={} from {}",
                Utc::now(),
                expected_seq,
                pkt.seq,
                gap,
                peer
            );
            met.consecutive_failures += gap as u32;
            met.missing_packets_count += gap as u32;

            for missing in expected_seq..pkt.seq {
                let deadline = Instant::now() + Duration::from_millis(URGENT_DEADLINE_MS);
                let _ = cmd_tx.try_send((Command::ReRequestTelemetry { seq: missing }, deadline));
            }
        }
    } else {
        if met.consecutive_failures > 0 {
            println!(
                "[TELEMETRY][{}] Link restored – resetting {} consecutive failures.",
                Utc::now(),
                met.consecutive_failures
            );
        }
        met.consecutive_failures = 0;
    }

    if met.consecutive_failures >= 3 {
        println!(
            "[TELEMETRY][{}] *** LOSS-OF-CONTACT: {} failures ***",
            Utc::now(),
            met.consecutive_failures
        );
    }

    if let Some(last) = met.last_recv_instant {
        let actual_interval = recv_instant.duration_since(last).as_millis() as i128;
        met.jitter_samples.push((actual_interval - expected_period_ms).abs());
    }

    if met.first_recv_instant.is_none() {
        met.first_recv_instant = Some(recv_instant);
        met.first_seq = Some(pkt.seq);
    }
    if let (Some(t0), Some(s0)) = (met.first_recv_instant, met.first_seq) {
        let elapsed_ms = recv_instant.duration_since(t0).as_millis() as i128;
        let expected_offset = (pkt.seq as i128 - s0 as i128) * expected_period_ms;
        met.drift_samples.push(elapsed_ms - expected_offset);
    }

    met.last_seq = Some(pkt.seq);
    met.last_recv_instant = Some(recv_instant);
    met.latency_samples.push(latency_ms as i128);
}

fn handle_fault_packet(pkt: &TelemetryPacket, state: &GcsState) {
    if let TelemetryType::Fault(ref desc) = pkt.ttype {
        let il_start = Instant::now();
        let is_new_fault = {
            let met = state.metrics.lock();
            met.active_fault.as_ref() != Some(desc)
        };

        if is_new_fault {
            println!(
                "[TELEMETRY][{}] FAULT seq={} desc='{}' – engaging interlock.",
                Utc::now(),
                pkt.seq,
                desc
            );
        } else {
            println!(
                "[TELEMETRY][{}] FAULT seq={} desc='{}' – fault still active.",
                Utc::now(),
                pkt.seq,
                desc
            );
        }
        {
            let mut met = state.metrics.lock();
            if is_new_fault {
                met.active_fault = Some(desc.clone());
                met.fault_detected_at = Some(Instant::now());
            }
        }
        *state.safe_mode.lock() = true;

        let il_us = il_start.elapsed().as_micros();
        state.metrics.lock().interlock_latency_us.push(il_us);
        println!(
            "[TELEMETRY][{}] Interlock engaged in {}µs.",
            Utc::now(),
            il_us
        );
    }
}

fn log_packet(pkt: &TelemetryPacket, latency_ms: i64, decode_ms: f64) {
    println!(
        "[TELEMETRY][{ts}] seq={seq:>6}  type={t:?}  latency={lat:>4}ms  decode={dec:.3}ms",
        ts  = Utc::now().format("%H:%M:%S%.3f"),
        seq = pkt.seq,
        t   = pkt.ttype,
        lat = latency_ms,
        dec = decode_ms,
    );
}
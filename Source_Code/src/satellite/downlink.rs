use std::time::{Duration, Instant};

use chrono::Utc;
use rand::{rngs::StdRng, Rng, SeedableRng};
use tokio::time;
use tokio_util::sync::CancellationToken;

use crate::satellite::state::{DownlinkPacket, SharedState, TX_QUEUE_CAPACITY};

const VISIBILITY_WINDOW_MS: u64 = 100;
const MAX_SAMPLES_PER_PACKET: usize = 16;

fn sample_init_delay_ms(rng: &mut StdRng) -> u64 {
    // Keep rare >5ms delays for missed-communication simulation,
    // but avoid overwhelming the system with synthetic misses.
    if rng.gen_bool(0.01) {
        rng.gen_range(6_u64..9_u64)
    } else {
        rng.gen_range(1_u64..5_u64)
    }
}

pub async fn run(state: SharedState, token: CancellationToken) {
    let mut rng = StdRng::from_entropy();
    let mut next_window = Instant::now() + Duration::from_millis(VISIBILITY_WINDOW_MS);

    loop {
        tokio::select! {
            _ = token.cancelled() => {
                println!("[DOWNLINK] shutdown.");
                break;
            }
            _ = time::sleep(Duration::from_millis(10)) => {}
        }

        let now = Instant::now();
        if now < next_window {
            let mut ocs = state.lock().await;
            let sensor_fill = ocs.sensor_fill_ratio();
            let tx_fill = ocs.tx_fill_ratio();
            ocs.buffer_fill_rates.push(sensor_fill);
            ocs.tx_fill_rates.push(tx_fill);
            continue;
        }

        next_window += Duration::from_millis(VISIBILITY_WINDOW_MS);
        let visibility_start = Instant::now();
        println!("[DOWNLINK][{}] visibility window opened", Utc::now());

        let init_delay_ms = sample_init_delay_ms(&mut rng);
        time::sleep(Duration::from_millis(init_delay_ms)).await;

        if init_delay_ms > 5 {
            let mut ocs = state.lock().await;
            ocs.downlink_init_missed += 1;
            println!(
                "[DOWNLINK][{}] missed communication: init {}ms (>5ms)",
                Utc::now(),
                init_delay_ms
            );
            continue;
        }

        let prep_start = Instant::now();
        let samples = {
            let mut ocs = state.lock().await;
            pop_highest_priority_samples(&mut ocs.sensor_buffer, MAX_SAMPLES_PER_PACKET)
        };

        if !samples.is_empty() {
            let ttype = classify_packet_type(&samples);
            let payload = compress_and_packetize_batch(&samples);
            let packet = DownlinkPacket {
                ttype,
                payload,
                enqueued_at: Instant::now(),
            };

            let mut ocs = state.lock().await;
            if ocs.tx_queue.len() < TX_QUEUE_CAPACITY {
                ocs.tx_queue.push_back(packet);
            } else {
                ocs.dropped_packets += 1;
                println!(
                    "[DOWNLINK][{}] tx queue full: dropping packet",
                    Utc::now()
                );
            }

            let prep_ms = prep_start.elapsed().as_millis();
            if prep_ms > 30 {
                ocs.downlink_prep_deadline_missed += 1;
                println!(
                    "[DOWNLINK][{}] prep deadline missed: {}ms (>30ms)",
                    Utc::now(),
                    prep_ms
                );
            }

            let vis_ms = Instant::now().duration_since(visibility_start).as_millis();
            println!(
                "[DOWNLINK][{}] prepared packet in {}ms (window={}ms)",
                Utc::now(),
                prep_ms,
                vis_ms
            );

            let sensor_fill = ocs.sensor_fill_ratio();
            if sensor_fill > 0.8 {
                if !ocs.degraded_mode {
                    ocs.degraded_mode = true;
                    println!(
                        "[DOWNLINK][{}] degraded mode ON: sensor buffer {:.0}%",
                        Utc::now(),
                        sensor_fill * 100.0
                    );
                }
            } else if ocs.degraded_mode {
                ocs.degraded_mode = false;
                println!(
                    "[DOWNLINK][{}] degraded mode OFF: sensor buffer {:.0}%",
                    Utc::now(),
                    sensor_fill * 100.0
                );
            }

            let tx_fill = ocs.tx_fill_ratio();
            ocs.buffer_fill_rates.push(sensor_fill);
            ocs.tx_fill_rates.push(tx_fill);
        }
    }
}

fn pop_highest_priority_samples(
    buffer: &mut std::collections::VecDeque<crate::satellite::state::SensorSample>,
    max_count: usize,
) -> Vec<crate::satellite::state::SensorSample> {
    let mut out = Vec::new();
    for _ in 0..max_count {
        let mut best_idx: Option<usize> = None;
        let mut best_pri = 0u8;

        for (idx, sample) in buffer.iter().enumerate() {
            let pri = sample.kind.priority();
            if pri > best_pri {
                best_pri = pri;
                best_idx = Some(idx);
            }
        }

        if let Some(i) = best_idx {
            if let Some(sample) = buffer.remove(i) {
                out.push(sample);
            }
        } else {
            break;
        }
    }

    out
}

fn classify_packet_type(samples: &[crate::satellite::state::SensorSample]) -> crate::shared::TelemetryType {
    if samples.iter().any(|s| matches!(s.kind, crate::satellite::state::SensorKind::Thermal)) {
        crate::shared::TelemetryType::Thermal
    } else if samples.iter().any(|s| matches!(s.kind, crate::satellite::state::SensorKind::Attitude)) {
        crate::shared::TelemetryType::Attitude
    } else {
        crate::shared::TelemetryType::Power
    }
}

fn compress_and_packetize_batch(samples: &[crate::satellite::state::SensorSample]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(1 + samples.len() * 4);
    payload.push(samples.len().min(u8::MAX as usize) as u8);

    for sample in samples.iter().take(u8::MAX as usize) {
        let quantized = (sample.value * 10.0) as i16;
        payload.extend_from_slice(&quantized.to_le_bytes());
        payload.push(match sample.kind {
            crate::satellite::state::SensorKind::Thermal => 0,
            crate::satellite::state::SensorKind::Attitude => 1,
            crate::satellite::state::SensorKind::Power => 2,
        });
        payload.push(if sample.corrupted { 1 } else { 0 });
    }

    payload
}

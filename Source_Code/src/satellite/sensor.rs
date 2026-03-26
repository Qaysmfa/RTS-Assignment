use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use rand::{rngs::StdRng, Rng, SeedableRng};
use tokio::runtime::Handle;
use tokio::time::{self, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::satellite::state::{OcsState, SensorKind, SensorSample, SharedState, SENSOR_BUFFER_CAPACITY};

pub async fn run(state: SharedState, token: CancellationToken) {
    let mut handles = Vec::new();

    for sensor in [SensorKind::Thermal, SensorKind::Attitude, SensorKind::Power] {
        let s = state.clone();
        let t = token.clone();
        if sensor == SensorKind::Thermal {
            let handle = Handle::current();
            handles.push(tokio::task::spawn_blocking(move || {
                thermal_sensor_loop_blocking(sensor, s, t, handle);
            }));
        } else {
            handles.push(tokio::spawn(async move {
                sensor_loop(sensor, s, t).await;
            }));
        }
    }

    for handle in handles {
        let _ = handle.await;
    }
}

async fn sensor_loop(kind: SensorKind, state: SharedState, token: CancellationToken) {
    let period = kind.period();
    let mut ticker = time::interval(period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    // Consume the immediate first tick so each sample aligns to one full period.
    ticker.tick().await;

    let mut next_expected = Instant::now() + period;
    let mut last_start: Option<Instant> = None;
    let mut rng = StdRng::from_entropy();

    loop {
        tokio::select! {
            _ = token.cancelled() => {
                println!("[SENSOR-{kind:?}] shutdown.");
                break;
            }
            _ = ticker.tick() => {
                let actual_start = Instant::now();
                let drift_us = actual_start.saturating_duration_since(next_expected).as_micros() as i128;

                {
                    let mut ocs = state.lock().await;
                    ocs.sensor_drift_us.push(drift_us);
                }

                if let Some(last) = last_start {
                    let actual_interval_us = actual_start.duration_since(last).as_micros() as i128;
                    let expected_interval_us = period.as_micros() as i128;
                    let jitter_us = actual_interval_us - expected_interval_us;
                    let mut ocs = state.lock().await;
                    ocs.sensor_jitter_us.push(jitter_us.abs());
                }
                last_start = Some(actual_start);

                let mut delayed = false;
                let mut corrupted = false;
                {
                    let mut ocs = state.lock().await;
                    if kind == SensorKind::Power && ocs.delayed_power_pending {
                        ocs.delayed_power_pending = false;
                        delayed = true;
                    }
                    if ocs.corrupt_next_sample {
                        ocs.corrupt_next_sample = false;
                        corrupted = true;
                    }
                }

                if delayed {
                    time::sleep(std::time::Duration::from_millis(6)).await;
                    println!(
                        "[SENSOR-POWER][{}] injected delayed sample (+6ms)",
                        Utc::now()
                    );
                }

                let read_at = Instant::now();
                let mut value = match kind {
                    SensorKind::Thermal => rng.gen_range(36.5_f32..82.0_f32),
                    SensorKind::Attitude => rng.gen_range(-5.0_f32..5.0_f32),
                    SensorKind::Power => rng.gen_range(20.0_f32..100.0_f32),
                };
                if corrupted {
                    value = 9999.0;
                }

                let inserted_at = Instant::now();
                let latency_us = inserted_at.duration_since(read_at).as_micros();

                let sample = SensorSample {
                    kind,
                    value,
                    read_at,
                    scheduled_at: next_expected,
                    inserted_at,
                    corrupted,
                };

                let inserted = insert_prioritized_sample(sample, &state).await;

                {
                    let mut ocs = state.lock().await;
                    ocs.sensor_latency_us.push(latency_us);

                    if kind == SensorKind::Thermal {
                        if inserted {
                            ocs.thermal_missed_cycles = 0;
                        } else {
                            ocs.thermal_missed_cycles += 1;
                            if ocs.thermal_missed_cycles > 3 {
                                let fault_text = "thermal data missed >3 cycles".to_string();
                                if ocs.active_fault.as_ref() != Some(&fault_text) {
                                    ocs.safety_alerts += 1;
                                    ocs.safe_mode = true;
                                    ocs.active_fault = Some(fault_text);
                                    ocs.active_fault_since = Some(Instant::now());
                                    ocs.fault_telemetry_pending = true;
                                    println!(
                                        "[SENSOR-THERMAL][{}] SAFETY ALERT: missed >3 cycles",
                                        Utc::now()
                                    );
                                }
                            }
                        }
                    }

                    let fill_ratio = ocs.sensor_fill_ratio();
                    ocs.buffer_fill_rates.push(fill_ratio);
                    if fill_ratio > 0.8 {
                        ocs.degraded_mode = true;
                        println!(
                            "[DOWNLINK][{}] degraded mode ON (sensor buffer {:.0}%)",
                            Utc::now(),
                            fill_ratio * 100.0
                        );
                    }
                }

                next_expected += period;
                let now = Instant::now();
                while next_expected <= now {
                    next_expected += period;
                }
            }
        }
    }
}

fn thermal_sensor_loop_blocking(
    kind: SensorKind,
    state: SharedState,
    token: CancellationToken,
    handle: Handle,
) {
    let period = kind.period();
    let mut next_expected = Instant::now() + period;
    let mut last_start: Option<Instant> = None;
    let mut last_drift_us: Option<i128> = None;
    let mut warmup_cycles_remaining: u32 = 8;
    let mut rng = StdRng::from_entropy();

    while !token.is_cancelled() {
        precise_sleep_until(next_expected);
        if token.is_cancelled() {
            break;
        }

        let actual_start = Instant::now();
        let drift_us = actual_start.saturating_duration_since(next_expected).as_micros() as i128;
        let mut delayed = false;
        let mut corrupted = false;
        if let Some(last) = last_start {
            let _actual_interval_us = actual_start.duration_since(last).as_micros() as i128;
            let jitter_us = last_drift_us
                .map(|prev| (drift_us - prev).abs())
                .unwrap_or(0);

            handle.block_on(async {
                let mut ocs = state.lock().await;
                ocs.sensor_drift_us.push(drift_us);
                if warmup_cycles_remaining == 0 {
                    ocs.sensor_jitter_us.push(jitter_us);
                    if jitter_us > 1_000 {
                        ocs.critical_jitter_violations += 1;
                    }
                }
                if ocs.corrupt_next_sample {
                    ocs.corrupt_next_sample = false;
                    corrupted = true;
                }
            });
        } else {
            handle.block_on(async {
                let mut ocs = state.lock().await;
                ocs.sensor_drift_us.push(drift_us);
                if ocs.corrupt_next_sample {
                    ocs.corrupt_next_sample = false;
                    corrupted = true;
                }
            });
        }

        if warmup_cycles_remaining > 0 {
            warmup_cycles_remaining -= 1;
        }
        last_drift_us = Some(drift_us);
        last_start = Some(actual_start);

        if kind == SensorKind::Power {
            handle.block_on(async {
                let mut ocs = state.lock().await;
                if ocs.delayed_power_pending {
                    ocs.delayed_power_pending = false;
                    delayed = true;
                }
            });
        }

        if delayed {
            thread::sleep(Duration::from_millis(6));
            println!(
                "[SENSOR-POWER][{}] injected delayed sample (+6ms)",
                Utc::now()
            );
        }

        let read_at = Instant::now();
        let mut value = match kind {
            SensorKind::Thermal => rng.gen_range(36.5_f32..82.0_f32),
            SensorKind::Attitude => rng.gen_range(-5.0_f32..5.0_f32),
            SensorKind::Power => rng.gen_range(20.0_f32..100.0_f32),
        };
        if corrupted {
            value = 9999.0;
        }

        let inserted_at = Instant::now();
        let latency_us = inserted_at.duration_since(read_at).as_micros();

        let sample = SensorSample {
            kind,
            value,
            read_at,
            scheduled_at: next_expected,
            inserted_at,
            corrupted,
        };

        handle.block_on(async {
            let mut ocs = state.lock().await;
            let inserted = insert_prioritized_sample_locked(sample, &mut ocs);
            ocs.sensor_latency_us.push(latency_us);

            if kind == SensorKind::Thermal {
                if inserted {
                    ocs.thermal_missed_cycles = 0;
                } else {
                    ocs.thermal_missed_cycles += 1;
                    if ocs.thermal_missed_cycles > 3 {
                        let fault_text = "thermal data missed >3 cycles".to_string();
                        if ocs.active_fault.as_ref() != Some(&fault_text) {
                            ocs.safety_alerts += 1;
                            ocs.safe_mode = true;
                            ocs.active_fault = Some(fault_text);
                            ocs.active_fault_since = Some(Instant::now());
                            ocs.fault_telemetry_pending = true;
                            println!(
                                "[SENSOR-THERMAL][{}] SAFETY ALERT: missed >3 cycles",
                                Utc::now()
                            );
                        }
                    }
                }
            }

            let fill_ratio = ocs.sensor_fill_ratio();
            ocs.buffer_fill_rates.push(fill_ratio);
            if fill_ratio > 0.8 {
                ocs.degraded_mode = true;
                println!(
                    "[DOWNLINK][{}] degraded mode ON (sensor buffer {:.0}%)",
                    Utc::now(),
                    fill_ratio * 100.0
                );
            }
        });

        next_expected += period;
        let now = Instant::now();
        while next_expected <= now {
            next_expected += period;
        }
    }

    println!("[SENSOR-{kind:?}] shutdown.");
}

fn precise_sleep_until(deadline: Instant) {
    const COARSE_MARGIN: Duration = Duration::from_micros(2_000);

    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }

        let remaining = deadline.duration_since(now);
        if remaining > COARSE_MARGIN {
            thread::sleep(remaining - COARSE_MARGIN);
        } else {
            std::hint::spin_loop();
        }
    }
}

async fn insert_prioritized_sample(sample: SensorSample, state: &SharedState) -> bool {
    let mut ocs = state.lock().await;

    insert_prioritized_sample_locked(sample, &mut ocs)
}

fn insert_prioritized_sample_locked(sample: SensorSample, ocs: &mut OcsState) -> bool {

    if ocs.sensor_buffer.len() < SENSOR_BUFFER_CAPACITY {
        ocs.sensor_buffer.push_back(sample);
        return true;
    }

    let incoming_p = sample.kind.priority();
    let mut lowest_idx: Option<usize> = None;
    let mut lowest_p = u8::MAX;

    for (idx, item) in ocs.sensor_buffer.iter().enumerate() {
        let p = item.kind.priority();
        if p < lowest_p {
            lowest_p = p;
            lowest_idx = Some(idx);
        }
    }

    if incoming_p > lowest_p {
        if let Some(idx) = lowest_idx {
            let dropped = ocs.sensor_buffer.remove(idx);
            ocs.sensor_buffer.push_back(sample);
            ocs.dropped_packets += 1;
            if let Some(d) = dropped {
                println!(
                    "[SENSOR-BUF][{}] dropped {:?} sample due to priority replacement",
                    Utc::now(),
                    d.kind
                );
            }
            true
        } else {
            false
        }
    } else {
        ocs.dropped_packets += 1;
        println!(
            "[SENSOR-BUF][{}] dropped incoming {:?} sample (buffer full)",
            Utc::now(),
            sample.kind
        );
        false
    }
}

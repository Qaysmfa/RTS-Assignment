use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use chrono::Utc;
use tokio::time;
use tokio_util::sync::CancellationToken;

use crate::satellite::state::SharedState;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum TaskKind {
    ThermalControl,
    Compression,
    HealthMonitoring,
    AntennaAlignment,
}

#[derive(Clone, Debug)]
struct Job {
    kind: TaskKind,
    release_at: Instant,
    deadline_at: Instant,
    remaining_quanta: u32,
    first_start: Option<Instant>,
}

#[derive(Clone, Debug)]
struct TaskDef {
    kind: TaskKind,
    period: Duration,
    exec_quanta: u32,
    deadline: Duration,
    next_release: Instant,
}

pub async fn run(state: SharedState, token: CancellationToken) {
    let now = Instant::now();
    let mut defs = vec![
        TaskDef {
            kind: TaskKind::Compression,
            period: Duration::from_millis(40),
            exec_quanta: 1,
            deadline: Duration::from_millis(200),
            next_release: now + Duration::from_millis(40),
        },
        TaskDef {
            kind: TaskKind::HealthMonitoring,
            period: Duration::from_millis(250),
            exec_quanta: 1,
            deadline: Duration::from_millis(3000),
            next_release: now + Duration::from_millis(250),
        },
        TaskDef {
            kind: TaskKind::AntennaAlignment,
            period: Duration::from_millis(180),
            exec_quanta: 1,
            deadline: Duration::from_millis(1500),
            next_release: now + Duration::from_millis(180),
        },
    ];

    let mut ready: VecDeque<Job> = VecDeque::new();
    let mut last_task_start: HashMap<TaskKind, Instant> = HashMap::new();

    loop {
        let tick_start = Instant::now();

        tokio::select! {
            _ = token.cancelled() => {
                println!("[SCHED] shutdown.");
                break;
            }
            _ = time::sleep(Duration::from_millis(1)) => {}
        }

        let now = Instant::now();
        for def in &mut defs {
            if now >= def.next_release {
                // Resynchronize release timestamps to wall-clock "now" to avoid
                // carrying stale expected release times after OS scheduling delays.
                let scheduled = now;
                def.next_release = now + def.period;

                ready.push_back(Job {
                    kind: def.kind,
                    release_at: scheduled,
                    deadline_at: scheduled + def.deadline,
                    remaining_quanta: def.exec_quanta,
                    first_start: None,
                });
            }
        }

        let thermal_pending = {
            let ocs = state.lock().await;
            ocs.thermal_missed_cycles > 0 || ocs.safe_mode
        };

        if thermal_pending && !ready.iter().any(|j| j.kind == TaskKind::ThermalControl) {
            if !ready.is_empty() {
                let mut ocs = state.lock().await;
                ocs.thermal_preemptions += 1;
                println!(
                    "[SCHED][{}] preemption: thermal control inserted ahead of lower-priority tasks",
                    Utc::now()
                );
            }
            ready.push_back(Job {
                kind: TaskKind::ThermalControl,
                release_at: now,
                deadline_at: now + Duration::from_millis(10),
                remaining_quanta: 2,
                first_start: None,
            });
        }

        if ready.is_empty() {
            let mut ocs = state.lock().await;
            ocs.cpu_idle_us += tick_start.elapsed().as_micros();
            continue;
        }

        let selected_idx = select_highest_priority_job(&ready);
        let mut job = ready.remove(selected_idx).expect("selected job must exist");

        if job.first_start.is_none() {
            job.first_start = Some(now);
            let start_delay_us = now.duration_since(job.release_at).as_micros() as i128;
            {
                let mut ocs = state.lock().await;
                ocs.schedule_drift_us.push(start_delay_us);
                if now > job.deadline_at {
                    ocs.deadline_violations += 1;
                    println!(
                        "[SCHED][{}] deadline violation (start): {:?}",
                        Utc::now(),
                        job.kind
                    );
                }
            }

            if let Some(last) = last_task_start.insert(job.kind, now) {
                let nominal_us = nominal_period_us(job.kind);
                let actual_us = now.duration_since(last).as_micros() as i128;
                let jitter = (actual_us - nominal_us).abs();
                state.lock().await.task_jitter_us.push(jitter);
            }
        }

        time::sleep(Duration::from_millis(1)).await;
        job.remaining_quanta = job.remaining_quanta.saturating_sub(1);

        {
            let mut ocs = state.lock().await;
            ocs.cpu_active_us += tick_start.elapsed().as_micros();
        }

        if job.remaining_quanta > 0 {
            ready.push_back(job);
            continue;
        }

        if Instant::now() > job.deadline_at {
            let mut ocs = state.lock().await;
            ocs.deadline_violations += 1;
            println!(
                "[SCHED][{}] deadline violation (completion): {:?}",
                Utc::now(),
                job.kind
            );
        }

        if job.kind == TaskKind::ThermalControl {
            let mut ocs = state.lock().await;
            ocs.thermal_missed_cycles = 0;
            if ocs.active_fault.is_none() {
                ocs.safe_mode = false;
            }
            println!("[SCHED][{}] thermal control executed", Utc::now());
        }
    }
}

fn select_highest_priority_job(ready: &VecDeque<Job>) -> usize {
    let mut best_idx = 0usize;
    let mut best_pri = u8::MAX;

    for (idx, job) in ready.iter().enumerate() {
        let pri = priority(job.kind);
        if pri < best_pri {
            best_pri = pri;
            best_idx = idx;
        }
    }

    best_idx
}

fn priority(kind: TaskKind) -> u8 {
    match kind {
        TaskKind::ThermalControl => 0,
        TaskKind::Compression => 1,
        TaskKind::HealthMonitoring => 2,
        TaskKind::AntennaAlignment => 3,
    }
}

fn nominal_period_us(kind: TaskKind) -> i128 {
    match kind {
        TaskKind::ThermalControl => 10_000,
        TaskKind::Compression => 40_000,
        TaskKind::HealthMonitoring => 250_000,
        TaskKind::AntennaAlignment => 180_000,
    }
}

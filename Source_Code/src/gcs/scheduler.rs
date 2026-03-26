use std::time::{Duration, Instant};

use chrono::Utc;
use tokio::sync::mpsc;
use tokio::time;

use crate::gcs::state::{CommandEntry, GcsState};
use crate::shared::Command;
use crate::gcs::uplink::URGENT_DEADLINE_MS;

pub async fn run(state: GcsState, sender: mpsc::Sender<(Command, Instant)>) {
    let mut id_counter: u64 = 1;
    let mut interval = time::interval(Duration::from_millis(400));
    let mut expected_tick = Instant::now() + Duration::from_millis(400);

    loop {
        interval.tick().await;
        let now = Instant::now();
        
        let tick_drift_ms = if expected_tick > now {
            (expected_tick - now).as_millis() as i128 * -1
        } else {
            (now - expected_tick).as_millis() as i128
        };
        
        state.metrics.lock().scheduler_drift_samples.push(tick_drift_ms);
        expected_tick = now + Duration::from_millis(400);

        let cmd = select_next_command(id_counter);
        let deadline = now + Duration::from_millis(match &cmd {
            Command::Urgent { .. } => URGENT_DEADLINE_MS,
            _                      => 400,
        });

        if let Command::Urgent { id, ref body } = cmd {
            if *state.safe_mode.lock() {
                reject_command(id, body, deadline, &cmd, &state);
                id_counter += 1;
                continue;
            }
        }

        dispatch_command(cmd, deadline, id_counter, &sender).await;
        id_counter += 1;
    }
}

fn select_next_command(id: u64) -> Command {
    if id % 5 == 0 {
        Command::Urgent {
            id,
            body: format!("attitude-correct-{id}"),
        }
    } else if id % 10 == 0 {
        Command::QueryStatus
    } else {
        Command::NoOp
    }
}

fn reject_command(
    id: u64,
    body: &str,
    deadline: Instant,
    cmd: &Command,
    state: &GcsState,
) {
    let reason = "safe_mode active – Urgent commands blocked by interlock";
    println!(
        "[CMD-SCHED][{}] INTERLOCK BLOCK: Urgent id={} body='{}'\n\
         \t  Rejection reason: {}",
        Utc::now(),
        id,
        body,
        reason
    );
    state.cmd_backlog.insert(
        id,
        CommandEntry {
            id,
            cmd_debug: format!("{cmd:?}"),
            deadline_instant: deadline,
            priority: 0,
            created_instant: Instant::now(),
            rejection_reason: Some(reason.to_string()),
        },
    );
}

async fn dispatch_command(
    cmd: Command,
    deadline: Instant,
    id: u64,
    sender: &mpsc::Sender<(Command, Instant)>,
) {
    if let Err(e) = sender.send((cmd.clone(), deadline)).await {
        eprintln!("[CMD-SCHED][{}] Channel send error id={id}: {e:?}", Utc::now());
    } else {
        let urgency = if matches!(cmd, Command::Urgent { .. }) { "URGENT" } else { "NORMAL" };
        println!(
            "[CMD-SCHED][{}] Scheduled {} ({:?})  deadline=now+{}ms",
            Utc::now(),
            urgency,
            cmd,
            if matches!(cmd, Command::Urgent { .. }) { URGENT_DEADLINE_MS } else { 500 }
        );
    }
}
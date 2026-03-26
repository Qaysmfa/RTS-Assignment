use std::time::{Duration, Instant};

use chrono::Utc;
use tokio::net::UdpSocket;
use tokio::time;
use tokio_util::sync::CancellationToken;

use crate::satellite::state::SharedState;
use crate::shared::{
    TelemetryPacket, TelemetryType, GCS_TELEMETRY_BIND, TELEMETRY_PERIOD_MS,
};

const BLACKOUT_DURATION_MS: u64 = 1_000;
const BLACKOUT_TRIGGER_SECS: u64 = 90;

pub async fn telemetry_sender(state: SharedState, token: CancellationToken) -> anyhow::Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    let gcs_addr: std::net::SocketAddr = GCS_TELEMETRY_BIND.parse()?;
    println!("[SAT-TEL] Sending telemetry to {}", GCS_TELEMETRY_BIND);

    let mut seq: u64 = 1;
    let mut ticker = time::interval(Duration::from_millis(TELEMETRY_PERIOD_MS));
    let run_start = Instant::now();
    let mut blackout_fired = false;

    loop {
        tokio::select! {
            _ = token.cancelled() => {
                println!("[SAT-TEL] shutdown.");
                break;
            }
            _ = ticker.tick() => {
                if !blackout_fired
                    && run_start.elapsed().as_secs() >= BLACKOUT_TRIGGER_SECS
                {
                    blackout_fired = true;
                    println!(
                        "[SAT-TEL][{}] *** LINK BLACKOUT START – pausing {}ms ***",
                        Utc::now(),
                        BLACKOUT_DURATION_MS
                    );
                    tokio::select! {
                        _ = token.cancelled() => {
                            println!("[SAT-TEL] shutdown.");
                            break;
                        }
                        _ = time::sleep(Duration::from_millis(BLACKOUT_DURATION_MS)) => {}
                    }
                    println!(
                        "[SAT-TEL][{}] *** LINK BLACKOUT END – resuming transmission ***",
                        Utc::now()
                    );
                }

                let (fault_desc, maybe_data_pkt) = {
                    let mut s = state.lock().await;
                    let active_fault = if s.fault_telemetry_pending {
                        s.fault_telemetry_pending = false;
                        s.active_fault.clone()
                    } else {
                        None
                    };
                    let maybe_downlink = s.tx_queue.pop_front();

                    if let Some(ref pkt) = maybe_downlink {
                        let qlat = Instant::now().duration_since(pkt.enqueued_at).as_micros();
                        s.tx_latency_us.push(qlat);
                    }

                    (active_fault, maybe_downlink)
                };

                let pkt = if let Some(desc) = fault_desc {
                    {
                        let mut s = state.lock().await;
                        s.fault_packets += 1;
                    }
                    TelemetryPacket {
                        seq,
                        ttype: TelemetryType::Fault(desc),
                        sent_ts_millis: Utc::now().timestamp_millis(),
                        payload: vec![],
                    }
                } else if let Some(dl) = maybe_data_pkt {
                    TelemetryPacket {
                        seq,
                        ttype: dl.ttype,
                        sent_ts_millis: Utc::now().timestamp_millis(),
                        payload: dl.payload,
                    }
                } else {
                    let ttype = match seq % 3 {
                        0 => TelemetryType::Thermal,
                        1 => TelemetryType::Attitude,
                        _ => TelemetryType::Power,
                    };
                    TelemetryPacket {
                        seq,
                        ttype,
                        sent_ts_millis: Utc::now().timestamp_millis(),
                        payload: vec![0u8; 8],
                    }
                };

                let bytes = bincode::serialize(&pkt)?;
                socket.send_to(&bytes, gcs_addr).await?;

                println!("[SAT-TEL][{}] Sent seq={} type={:?}", Utc::now(), seq, pkt.ttype);
                state.lock().await.packets_sent += 1;
                seq += 1;
            }
        }
    }

    Ok(())
}

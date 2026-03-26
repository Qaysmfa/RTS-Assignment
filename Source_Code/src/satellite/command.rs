use chrono::Utc;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use crate::satellite::state::SharedState;
use crate::shared::{Command, CommandAck, OCS_CMD_BIND};

pub async fn command_listener(state: SharedState, token: CancellationToken) -> anyhow::Result<()> {
    let listener = TcpListener::bind(OCS_CMD_BIND).await?;
    println!("[SAT-CMD] TCP listener on {}", OCS_CMD_BIND);

    loop {
        tokio::select! {
            _ = token.cancelled() => {
                println!("[SAT-CMD] shutdown.");
                break;
            }
            accept_result = listener.accept() => {
                let (socket, peer) = accept_result?;
                println!("[SAT-CMD][{}] Connection from {}", Utc::now(), peer);

                let s = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_command_connection(socket, s).await {
                        eprintln!("[SAT-CMD] connection error: {e:?}");
                    }
                });
            }
        }
    }

    Ok(())
}

async fn handle_command_connection(mut socket: TcpStream, state: SharedState) -> anyhow::Result<()> {
    let started = Instant::now();

    let mut len_buf = [0u8; 4];
    if socket.read_exact(&mut len_buf).await.is_err() {
        return Ok(());
    }

    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    socket.read_exact(&mut buf).await?;

    let cmd: Command = match bincode::deserialize(&buf) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[SAT-CMD] deserialize error: {e:?}");
            return Ok(());
        }
    };

    println!("[SAT-CMD][{}] Received: {:?}", Utc::now(), cmd);

    let ack = process_command(cmd, &state).await;
    let ack_bytes = bincode::serialize(&ack)?;

    socket.write_all(&(ack_bytes.len() as u32).to_be_bytes()).await?;
    socket.write_all(&ack_bytes).await?;

    let resp_latency = started.elapsed().as_micros();
    state.lock().await.command_response_latency_us.push(resp_latency);

    println!("[SAT-CMD][{}] Sent ACK: {:?}", Utc::now(), ack);
    Ok(())
}

async fn process_command(cmd: Command, state: &SharedState) -> CommandAck {
    let mut ocs = state.lock().await;
    ocs.commands_received += 1;

    let ack = match cmd {
        Command::NoOp => CommandAck::Ok { id: 0 },

        Command::Urgent { id, body } => {
            if ocs.safe_mode {
                CommandAck::Rejected {
                    id,
                    reason: format!(
                        "OCS safe mode active (fault={:?}); urgent blocked",
                        ocs.active_fault
                    ),
                }
            } else {
                println!("[SAT-CMD] Executing urgent id={id} body={body}");
                CommandAck::Ok { id }
            }
        }

        Command::ReRequestTelemetry { seq } => {
            println!("[SAT-CMD] Re-request telemetry seq={seq}");
            CommandAck::Ok { id: seq }
        }

        Command::ClearFault => {
            println!("[SAT-CMD] Clearing fault (was {:?})", ocs.active_fault);
            if let Some(since) = ocs.active_fault_since {
                ocs.fault_recovery_us.push(since.elapsed().as_micros());
                ocs.fault_responses += 1;
            }
            ocs.active_fault = None;
            ocs.active_fault_since = None;
            ocs.fault_telemetry_pending = false;
            ocs.safe_mode = false;
            CommandAck::Ok { id: 0 }
        }

        Command::QueryStatus => CommandAck::StatusReport {
            safe_mode: ocs.safe_mode,
            active_fault: ocs.active_fault.clone(),
            uptime_secs: ocs.uptime_secs(),
        },
    };

    match ack {
        CommandAck::Rejected { .. } => ocs.ack_rejected += 1,
        _ => ocs.ack_ok += 1,
    }

    ack
}

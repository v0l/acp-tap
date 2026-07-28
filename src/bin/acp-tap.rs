//! `acp-tap` — transparent wrapper around an ACP agent.
//!
//! Sits between an ACP client (editor, harness) and an ACP agent, forwarding
//! stdio byte-for-byte while mirroring each complete line to `acp-tapd`.
//!
//! The overriding rule: **never disturb the agent**. Mirroring is best-effort
//! over a bounded channel — if the dashboard is absent, slow, or wedged, frames
//! are dropped and forwarding continues at full speed.
//!
//! ```text
//! client ──stdin──▶ acp-tap ──▶ agent
//! client ◀─stdout── acp-tap ◀── agent
//!                      └──▶ unix socket ──▶ acp-tapd
//! ```

use std::process::Stdio;

use acp_tap::{Direction, WireFrame, now_ms};
use anyhow::{Context, Result};
use clap::Parser;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    process::Command,
    sync::mpsc,
};

/// Bounded mirror queue. Full queue means the dashboard is not keeping up, and
/// dropping telemetry is always preferable to stalling the agent.
const MIRROR_QUEUE: usize = 1024;

#[derive(Parser, Debug)]
#[command(
    name = "acp-tap",
    about = "Transparent ACP proxy that mirrors JSON-RPC traffic to acp-tapd",
    long_about = None
)]
struct Args {
    /// Label for this agent in the dashboard. Defaults to $ACP_TAP_LABEL, then
    /// the basename of the working directory.
    #[arg(long, env = "ACP_TAP_LABEL")]
    label: Option<String>,

    /// Dashboard socket. Defaults to $XDG_RUNTIME_DIR/acp-tap.sock.
    #[arg(long, env = "ACP_TAP_SOCKET")]
    socket: Option<String>,

    /// The ACP agent command and its arguments, after `--`.
    #[arg(trailing_var_arg = true, required = true)]
    command: Vec<String>,
}

fn default_socket() -> String {
    match std::env::var("XDG_RUNTIME_DIR") {
        Ok(dir) if !dir.is_empty() => format!("{dir}/acp-tap.sock"),
        _ => "/tmp/acp-tap.sock".to_string(),
    }
}

fn default_label() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "agent".to_string())
}

/// Ship mirrored frames to the dashboard, reconnecting with backoff.
///
/// Runs detached from the forwarding path; every failure mode ends in "drop the
/// frame and try again later".
async fn mirror_task(socket: String, mut rx: mpsc::Receiver<WireFrame>) {
    let mut stream: Option<UnixStream> = None;
    let mut backoff_ms = 250u64;

    while let Some(frame) = rx.recv().await {
        let Ok(mut payload) = serde_json::to_vec(&frame) else {
            continue;
        };
        payload.push(b'\n');

        // Two attempts: a dashboard restart kills the existing connection, and
        // the failing write is typically the first frame of a new turn. Dropping
        // it would silently lose the prompt that started the turn.
        for attempt in 0..2 {
            if stream.is_none() {
                match UnixStream::connect(&socket).await {
                    Ok(s) => {
                        stream = Some(s);
                        backoff_ms = 250;
                    }
                    Err(_) => {
                        // Dashboard absent: throttle, then drop the frame.
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                        backoff_ms = (backoff_ms * 2).min(10_000);
                        break;
                    }
                }
            }

            let Some(s) = stream.as_mut() else { break };
            if s.write_all(&payload).await.is_ok() {
                break;
            }

            // Stale connection: reconnect and retry this same frame once.
            stream = None;
            let _ = attempt;
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();

    let label = args.label.unwrap_or_else(default_label);
    let socket = args.socket.unwrap_or_else(default_socket);

    let (program, rest) = args
        .command
        .split_first()
        .context("no agent command given; pass it after `--`")?;

    let mut child = Command::new(program)
        .args(rest)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit()) // agent diagnostics flow through untouched
        .spawn()
        .with_context(|| format!("failed to spawn agent: {program}"))?;

    let mut child_stdin = child.stdin.take().context("agent stdin unavailable")?;
    let child_stdout = child.stdout.take().context("agent stdout unavailable")?;

    let (tx, rx) = mpsc::channel::<WireFrame>(MIRROR_QUEUE);
    tokio::spawn(mirror_task(socket, rx));

    // client → agent
    let to_agent = {
        let tx = tx.clone();
        let label = label.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(tokio::io::stdin()).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if child_stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if child_stdin.write_all(b"\n").await.is_err() {
                    break;
                }
                let _ = child_stdin.flush().await;

                // try_send: never block the agent on a slow dashboard.
                let _ = tx.try_send(WireFrame {
                    label: label.clone(),
                    dir: Direction::ToAgent,
                    ts_ms: now_ms(),
                    line,
                });
            }
        })
    };

    // agent → client
    let to_client = {
        let label = label.clone();
        tokio::spawn(async move {
            let mut stdout = tokio::io::stdout();
            let mut lines = BufReader::new(child_stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if stdout.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stdout.write_all(b"\n").await.is_err() {
                    break;
                }
                let _ = stdout.flush().await;

                let _ = tx.try_send(WireFrame {
                    label: label.clone(),
                    dir: Direction::ToClient,
                    ts_ms: now_ms(),
                    line,
                });
            }
        })
    };

    let status = child.wait().await?;
    to_agent.abort();
    to_client.abort();

    std::process::exit(status.code().unwrap_or(0));
}

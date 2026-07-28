//! `acp-tapd` — dashboard daemon for `acp-tap`.
//!
//! Accepts mirrored frames on a unix socket, parses them into UI events, keeps a
//! bounded history, and streams everything to browsers over a websocket.

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use acp_tap::{AgentSnapshot, EventKind, UiEvent, WireFrame, parse_frame};
use anyhow::Result;
use axum::{
    Json, Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::{Html, IntoResponse},
    routing::get,
};
use clap::Parser;
use futures_util::{sink::SinkExt, stream::StreamExt};
use serde::Serialize;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{Mutex, broadcast},
};

#[derive(Parser, Debug)]
#[command(name = "acp-tapd", about = "Live dashboard for ACP agent sessions")]
struct Args {
    /// Socket that `acp-tap` connects to.
    #[arg(long, env = "ACP_TAP_SOCKET")]
    socket: Option<String>,

    /// Address for the web dashboard.
    #[arg(long, env = "ACP_TAPD_LISTEN", default_value = "127.0.0.1:9111")]
    listen: String,

    /// Number of events retained per agent.
    #[arg(long, env = "ACP_TAPD_HISTORY", default_value_t = 500)]
    history: usize,
}

struct AppState {
    agents: Mutex<HashMap<String, AgentSnapshot>>,
    history: Mutex<VecDeque<UiEvent>>,
    history_cap: usize,
    seq: AtomicU64,
    tx: broadcast::Sender<Broadcast>,
}

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Broadcast {
    Event(UiEvent),
    Agent(AgentSnapshot),
}

#[derive(Serialize)]
struct Snapshot {
    #[serde(rename = "type")]
    kind: &'static str,
    agents: Vec<AgentSnapshot>,
    events: Vec<UiEvent>,
}

impl AppState {
    /// Fold one parsed event into agent state, then broadcast both.
    async fn ingest(&self, label: &str, ts_ms: u64, session_id: Option<String>, event: EventKind) {
        let snapshot = {
            let mut agents = self.agents.lock().await;
            let agent = agents
                .entry(label.to_string())
                .or_insert_with(|| AgentSnapshot {
                    label: label.to_string(),
                    connected: true,
                    ..Default::default()
                });

            agent.connected = true;
            agent.last_activity_ms = Some(ts_ms);
            if session_id.is_some() {
                agent.session_id = session_id.clone();
            }

            match &event {
                EventKind::TurnStarted { .. } => {
                    agent.turn_started_ms = Some(ts_ms);
                    agent.turns += 1;
                }
                EventKind::TurnEnded { .. } => agent.turn_started_ms = None,
                EventKind::ToolCall { .. } => agent.tool_calls += 1,
                EventKind::SessionCreated { session_id } => {
                    agent.session_id = Some(session_id.clone())
                }
                _ => {}
            }

            agent.clone()
        };

        let ui = UiEvent {
            seq: self.seq.fetch_add(1, Ordering::Relaxed),
            label: label.to_string(),
            ts_ms,
            session_id,
            event,
        };

        {
            let mut history = self.history.lock().await;
            if history.len() >= self.history_cap {
                history.pop_front();
            }
            history.push_back(ui.clone());
        }

        let _ = self.tx.send(Broadcast::Event(ui));
        let _ = self.tx.send(Broadcast::Agent(snapshot));
    }

    async fn mark_disconnected(&self, label: &str) {
        let snapshot = {
            let mut agents = self.agents.lock().await;
            match agents.get_mut(label) {
                Some(agent) => {
                    agent.connected = false;
                    agent.turn_started_ms = None;
                    agent.clone()
                }
                None => return,
            }
        };
        let _ = self.tx.send(Broadcast::Agent(snapshot));
    }
}

/// One connected `acp-tap` process.
async fn handle_tap(state: Arc<AppState>, stream: UnixStream) {
    let mut lines = BufReader::new(stream).lines();
    let mut label: Option<String> = None;

    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(frame) = serde_json::from_str::<WireFrame>(&line) else {
            continue;
        };
        if label.is_none() {
            label = Some(frame.label.clone());
        }

        for (session_id, event) in parse_frame(&frame) {
            state
                .ingest(&frame.label, frame.ts_ms, session_id, event)
                .await;
        }
    }

    if let Some(label) = label {
        state.mark_disconnected(&label).await;
    }
}

async fn ws_handler(State(state): State<Arc<AppState>>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(|socket| ws_connection(state, socket))
}

async fn ws_connection(state: Arc<AppState>, socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();
    let mut rx = state.tx.subscribe();

    // Replay current state so a late browser sees history immediately.
    let snapshot = Snapshot {
        kind: "snapshot",
        agents: state.agents.lock().await.values().cloned().collect(),
        events: state.history.lock().await.iter().cloned().collect(),
    };
    if let Ok(text) = serde_json::to_string(&snapshot)
        && sender.send(Message::Text(text.into())).await.is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(update) => {
                    let Ok(text) = serde_json::to_string(&update) else { continue };
                    if sender.send(Message::Text(text.into())).await.is_err() {
                        return;
                    }
                }
                // Lagged: the browser fell behind. Keep going; history covers the gap.
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return,
            },
            incoming = receiver.next() => match incoming {
                Some(Ok(_)) => {}
                _ => return,
            },
        }
    }
}

async fn agents_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let agents: Vec<AgentSnapshot> = state.agents.lock().await.values().cloned().collect();
    Json(agents)
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../../static/index.html"))
}

fn default_socket() -> String {
    match std::env::var("XDG_RUNTIME_DIR") {
        Ok(dir) if !dir.is_empty() => format!("{dir}/acp-tap.sock"),
        _ => "/tmp/acp-tap.sock".to_string(),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "acp_tapd=info,info".into()),
        )
        .init();

    let args = Args::parse();
    let socket_path = args.socket.unwrap_or_else(default_socket);

    let (tx, _) = broadcast::channel(4096);
    let state = Arc::new(AppState {
        agents: Mutex::new(HashMap::new()),
        history: Mutex::new(VecDeque::new()),
        history_cap: args.history,
        seq: AtomicU64::new(1),
        tx,
    });

    // A stale socket from an unclean shutdown would block binding.
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)?;
    tracing::info!(socket = %socket_path, "listening for taps");

    {
        let state = state.clone();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        tokio::spawn(handle_tap(state.clone(), stream));
                    }
                    Err(e) => tracing::warn!("tap accept failed: {e}"),
                }
            }
        });
    }

    let app = Router::new()
        .route("/", get(index))
        .route("/api/agents", get(agents_handler))
        .route("/ws", get(ws_handler))
        .with_state(state);

    let tcp = tokio::net::TcpListener::bind(&args.listen).await?;
    tracing::info!(listen = %args.listen, "dashboard ready at http://{}", args.listen);
    axum::serve(tcp, app).await?;

    Ok(())
}

//! A minimal Discord *Interactions Endpoint* server.
//!
//! Discord delivers every interaction over HTTPS as a signed POST. This server:
//!   1. Verifies the Ed25519 request signature (required by Discord).
//!   2. Replies to the `PING` handshake with `PONG`.
//!   3. Replies to the `/ping` slash command with a detailed latency breakdown.
//!
//! The latency we report is derived from the interaction *snowflake* id, whose
//! high bits encode the millisecond Discord created the interaction. Comparing
//! that against our wall clock gives the Discord -> server delivery time.

use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::{json, Value};

/// Discord's custom epoch (2015-01-01T00:00:00Z) in Unix milliseconds.
const DISCORD_EPOCH_MS: u64 = 1_420_070_400_000;

/// Message flag: only the invoking user sees the reply.
const FLAG_EPHEMERAL: u64 = 1 << 6;
/// Message flag: opts the message into the components-v2 layout system
/// (container/text-display/separator). Mutually exclusive with `content`
/// and `embeds`.
const FLAG_IS_COMPONENTS_V2: u64 = 1 << 15;

#[derive(Clone)]
struct AppState {
    verifying_key: VerifyingKey,
}

#[tokio::main]
async fn main() {
    // Load variables from a local .env file if present (real env vars win).
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ping_pong=info,tower_http=info".into()),
        )
        .init();

    // The application's public key, copied from the Discord Developer Portal.
    let public_key_hex =
        std::env::var("DISCORD_PUBLIC_KEY").expect("DISCORD_PUBLIC_KEY env var must be set");
    let public_key_bytes: [u8; 32] = hex::decode(public_key_hex.trim())
        .expect("DISCORD_PUBLIC_KEY must be valid hex")
        .try_into()
        .expect("DISCORD_PUBLIC_KEY must decode to exactly 32 bytes");
    let verifying_key =
        VerifyingKey::from_bytes(&public_key_bytes).expect("DISCORD_PUBLIC_KEY is not a valid key");

    let state = AppState { verifying_key };

    let app = Router::new()
        .route("/", get(health))
        .route("/health", get(health))
        .route("/interactions", post(interactions))
        .with_state(state);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    // HOST lets the compose file pin the bind to loopback when the container
    // runs with host networking (public traffic must only enter via Caddy).
    let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into());
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .expect("HOST/PORT must form a valid socket address");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind listener");
    tracing::info!("ping-pong listening on http://{addr}/interactions");

    axum::serve(listener, app)
        .await
        .expect("server error");
}

/// Liveness probe — handy for container orchestrators and quick manual checks.
async fn health() -> impl IntoResponse {
    (StatusCode::OK, "pong server is alive\n")
}

/// The Discord interactions webhook.
async fn interactions(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    // Start the handling clock the instant the body lands.
    let received_at = Instant::now();

    if !verify_signature(&state.verifying_key, &headers, &body) {
        tracing::warn!("rejected interaction: invalid signature");
        return (StatusCode::UNAUTHORIZED, "invalid request signature").into_response();
    }

    let payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("rejected interaction: bad JSON: {e}");
            return (StatusCode::BAD_REQUEST, "invalid JSON body").into_response();
        }
    };

    match payload.get("type").and_then(Value::as_u64) {
        // 1 = PING. Discord sends this to verify the endpoint URL and as a keepalive.
        Some(1) => {
            tracing::info!("handled PING -> PONG");
            Json(json!({ "type": 1 })).into_response()
        }
        // 2 = APPLICATION_COMMAND (slash command), 3 = MESSAGE_COMPONENT (button click).
        // Both reply with a fresh latency report as a new message + a "Ping again" button.
        Some(2) | Some(3) => {
            // Snapshot handling time before the outbound probes so the report
            // reflects verify+parse work, not the probes' round trips.
            let handling_ms = received_at.elapsed().as_secs_f64() * 1000.0;
            // Caddy forwards the original client (Discord's webhook sender) here.
            let source_ip = headers
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.split(',').next())
                .map(|s| s.trim().to_string());
            let (edge_rtt_ms, link_rtt_ms) = tokio::join!(
                measure_discord_rtt(),
                webhook_link_rtt(source_ip.as_deref())
            );
            let components =
                latency_report(&payload, handling_ms, edge_rtt_ms, link_rtt_ms, source_ip);
            // 4 = CHANNEL_MESSAGE_WITH_SOURCE.
            Json(json!({
                "type": 4,
                "data": {
                    "components": components,
                    "flags": FLAG_EPHEMERAL | FLAG_IS_COMPONENTS_V2,
                }
            }))
            .into_response()
        }
        other => {
            tracing::warn!("unsupported interaction type: {other:?}");
            (StatusCode::BAD_REQUEST, "unsupported interaction type").into_response()
        }
    }
}

/// Build the 🏓 latency report as a components-v2 card from any interaction
/// payload that carries a snowflake `id` (slash commands and component clicks
/// both do). The container accent color and status dot track the headline
/// Discord -> server delivery time; diagnostic details (interaction id,
/// webhook source IP) sit in small print at the bottom of the card.
fn latency_report(
    payload: &Value,
    handling_ms: f64,
    edge_rtt_ms: Option<f64>,
    link_rtt_ms: Option<f64>,
    source_ip: Option<String>,
) -> Value {
    let now_ms = unix_millis() as i64;
    let now_secs = now_ms / 1000;

    let interaction_id = payload
        .get("id")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<u64>().ok());

    // Discord -> server delivery latency, derived from the snowflake.
    let delivery_ms = interaction_id.map(|id| now_ms - snowflake_timestamp_ms(id) as i64);

    // Discord brand palette: green / yellow / red / greyple.
    let (color, status) = match delivery_ms {
        Some(ms) if ms <= 150 => (0x57F287, "🟢"),
        Some(ms) if ms <= 300 => (0xFEE75C, "🟡"),
        Some(_) => (0xED4245, "🔴"),
        None => (0x99AAB5, "⚪"),
    };

    let delivery_str = match delivery_ms {
        Some(ms) => format!("{status} **{ms} ms** Discord → Server"),
        None => format!("{status} Discord → Server: n/a"),
    };
    let source_str = source_ip.unwrap_or_else(|| "n/a".into());
    let id_str = interaction_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| "n/a".into());

    tracing::info!(
        interaction_id = %id_str,
        delivery_ms = ?delivery_ms,
        link_rtt_ms = ?link_rtt_ms,
        edge_rtt_ms = ?edge_rtt_ms,
        source_ip = %source_str,
        handling_ms = handling_ms,
        "handled latency request"
    );

    // Component types: 17 = CONTAINER, 10 = TEXT_DISPLAY, 14 = SEPARATOR.
    json!([
        {
            "type": 17,
            "accent_color": color,
            "components": [
                {
                    "type": 10,
                    "content": format!(
                        "## 🏓 Pong!\n{delivery_str}\n-# one-way delivery, snowflake → arrival"
                    )
                },
                { "type": 14, "divider": true, "spacing": 1 },
                {
                    "type": 10,
                    "content": format!(
                        "🔗 Webhook link · **{link}**\n-# kernel TCP RTT, live connection\n\
                         🌐 API edge · **{edge}**\n-# TCP connect, Cloudflare\n\
                         ⚙️ Server handling · **{handling}**\n-# signature verify + parse",
                        link = fmt_ms(link_rtt_ms),
                        edge = fmt_ms(edge_rtt_ms),
                        handling = fmt_ms(Some(handling_ms)),
                    )
                },
                { "type": 14, "divider": true, "spacing": 1 },
                {
                    "type": 10,
                    "content": format!(
                        "-# Measured <t:{now_secs}:T> (<t:{now_secs}:R>) • \
                         Interaction {id_str} • Webhook source {source_str}"
                    )
                },
                ping_again_row(),
            ]
        }
    ])
}

/// Format a millisecond reading with precision that matches its magnitude:
/// microsecond detail only matters for sub-millisecond values.
fn fmt_ms(ms: Option<f64>) -> String {
    match ms {
        Some(ms) if ms < 1.0 => format!("{ms:.3} ms"),
        Some(ms) => format!("{ms:.1} ms"),
        None => "n/a".to_string(),
    }
}

/// Read the kernel-measured smoothed RTT of the live TCP connection from the
/// webhook sender — the true network distance to the machine that delivered
/// this very interaction. Works because the container shares the host network
/// namespace: the connection Discord holds to Caddy is visible to `ss` here
/// even though the app itself sits behind the proxy on loopback.
async fn webhook_link_rtt(source_ip: Option<&str>) -> Option<f64> {
    // Parse strictly as an IP before using it as a filter argument.
    let ip: std::net::IpAddr = source_ip?.parse().ok()?;
    let output = tokio::process::Command::new("ss")
        .args(["-Htin", "dst", &ip.to_string()])
        .output()
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    // `ss -i` emits e.g. `... rtt:1.427/1.672 ...` — smoothed rtt / variance.
    let rest = &text[text.find("rtt:")? + 4..];
    rest[..rest.find('/')?].parse().ok()
}

/// Measure the network round trip toward Discord by timing a TCP handshake to
/// `discord.com:443`. This lands on Discord's public front door (Cloudflare
/// edge) — the path outbound API calls take — NOT the webhook sender fleet,
/// which silently drops inbound SYNs and so cannot be probed actively. DNS
/// resolution happens before the clock starts so the figure is pure connect
/// RTT; the probe is capped so a network hiccup can't stall the response.
async fn measure_discord_rtt() -> Option<f64> {
    let probe = async {
        let addr = tokio::net::lookup_host("discord.com:443")
            .await
            .ok()?
            .next()?;
        let started = Instant::now();
        tokio::net::TcpStream::connect(addr).await.ok()?;
        Some(started.elapsed().as_secs_f64() * 1000.0)
    };
    tokio::time::timeout(Duration::from_millis(300), probe)
        .await
        .ok()
        .flatten()
}

/// The action row holding the "Ping again" button, nested inside the report
/// card so the button renders as part of the container.
fn ping_again_row() -> Value {
    json!({
        "type": 1, // ACTION_ROW
        "components": [
            {
                "type": 2,            // BUTTON
                "style": 1,           // PRIMARY
                "label": "Ping again",
                "custom_id": "ping_again"
            }
        ]
    })
}

/// Verify the `Ed25519` signature Discord attaches to every request.
///
/// The signed message is the concatenation of the `X-Signature-Timestamp`
/// header and the raw request body, in that order.
fn verify_signature(key: &VerifyingKey, headers: &HeaderMap, body: &[u8]) -> bool {
    let sig_hex = match headers
        .get("X-Signature-Ed25519")
        .and_then(|v| v.to_str().ok())
    {
        Some(s) => s,
        None => return false,
    };
    let timestamp = match headers
        .get("X-Signature-Timestamp")
        .and_then(|v| v.to_str().ok())
    {
        Some(s) => s,
        None => return false,
    };

    let sig_bytes: [u8; 64] = match hex::decode(sig_hex).ok().and_then(|b| b.try_into().ok()) {
        Some(b) => b,
        None => return false,
    };
    let signature = Signature::from_bytes(&sig_bytes);

    let mut message = Vec::with_capacity(timestamp.len() + body.len());
    message.extend_from_slice(timestamp.as_bytes());
    message.extend_from_slice(body);

    key.verify(&message, &signature).is_ok()
}

/// Extract the creation time (Unix ms) encoded in a Discord snowflake id.
fn snowflake_timestamp_ms(id: u64) -> u64 {
    (id >> 22) + DISCORD_EPOCH_MS
}

/// Current Unix time in milliseconds.
fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snowflake_decodes_to_known_timestamp() {
        // First-ever valid snowflake (id with all low bits zero) maps to the epoch.
        assert_eq!(snowflake_timestamp_ms(0), DISCORD_EPOCH_MS);
        // One millisecond after the epoch == 1 << 22.
        assert_eq!(snowflake_timestamp_ms(1 << 22), DISCORD_EPOCH_MS + 1);
    }
}

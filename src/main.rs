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
use std::time::{Instant, SystemTime, UNIX_EPOCH};

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
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

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
        // 2 = APPLICATION_COMMAND (a slash command was invoked).
        Some(2) => {
            let now_ms = unix_millis() as i64;

            let interaction_id = payload
                .get("id")
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<u64>().ok());

            // Discord -> server delivery latency, derived from the snowflake.
            let delivery_ms = interaction_id.map(|id| now_ms - snowflake_timestamp_ms(id) as i64);

            // How long *we* spent verifying + parsing this request.
            let handling = received_at.elapsed();
            let handling_ms = handling.as_secs_f64() * 1000.0;

            let delivery_str = match delivery_ms {
                Some(ms) => format!("{ms} ms"),
                None => "n/a".to_string(),
            };
            let id_str = interaction_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "n/a".into());

            tracing::info!(
                interaction_id = %id_str,
                delivery_ms = ?delivery_ms,
                handling_ms = handling_ms,
                "handled /ping"
            );

            let content = format!(
                concat!(
                    "🏓 **Pong!**\n",
                    "```\n",
                    "Discord -> server : {delivery}\n",
                    "Server handling   : {handling:.3} ms\n",
                    "Interaction id    : {id}\n",
                    "Measured at       : {now} (unix ms)\n",
                    "```"
                ),
                delivery = delivery_str,
                handling = handling_ms,
                id = id_str,
                now = now_ms,
            );

            // 4 = CHANNEL_MESSAGE_WITH_SOURCE.
            Json(json!({
                "type": 4,
                "data": { "content": content }
            }))
            .into_response()
        }
        other => {
            tracing::warn!("unsupported interaction type: {other:?}");
            (StatusCode::BAD_REQUEST, "unsupported interaction type").into_response()
        }
    }
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

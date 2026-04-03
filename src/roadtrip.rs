use std::collections::HashMap;

use anyhow::Context;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde::Deserialize;
use serde_aux::prelude::*;
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

/// Events emitted by the IRT backend
#[derive(Clone, Debug)]
pub enum RoadtripEvent {
    /// New data came in from the websocket
    WS(WSEvent),
}

/// A vote option, aka, an arrow
#[derive(Debug, Clone, Deserialize, Default)]
pub struct VoteOption {
    pub description: Option<String>,
    pub heading: f64,
    pub pano: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WSEvent {
    pub pano: String,
    pub heading: f64,
    pub location: Location,
    #[serde(deserialize_with = "deserialize_number_from_string")]
    pub total_users: u16,
    pub vote_counts: HashMap<i8, u16>,
    pub options: Vec<VoteOption>,
    #[serde(deserialize_with = "deserialize_datetime_utc_from_milliseconds")]
    pub end_time: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Location {
    pub road: String,
    pub neighborhood: String,
    pub state: String,
    pub county: String,
    pub country: String,
}

/// Websocket client that connects to the IRT websocket
pub struct WSBackend {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl WSBackend {
    pub async fn new() -> Result<Self, anyhow::Error> {
        let (socket, _response) =
            connect_async("wss://internet-roadtrip-listen-eqzms.ondigitalocean.app/")
                .await
                .context("Failed to connect to websocket")?;

        Ok(Self { socket })
    }
}

impl WSBackend {
    pub async fn next(&mut self) -> Option<anyhow::Result<WSEvent>> {
        let maybe_message = self.socket.next().await?;
        let result =
            || -> anyhow::Result<WSEvent> { Ok(serde_json::from_str(maybe_message?.to_text()?)?) };

        Some(result())
    }
}

#[test]
fn test_ws_event_deserialization() {
    let json = r#"
    {
        "pano": "abc123",
        "heading": 42.5,
        "location": {
            "road": "Tremont St",
            "neighborhood": "Boston",
            "state": "Massachusetts",
            "county": "Boston County",
            "country": "USA"
        },
        "totalUsers": "123",
        "voteCounts": { "1": 10, "-1": 5 },
        "options": [
            { "heading": 10.0, "pano": "p1" }
        ],
        "endTime": 1000
    }
    "#;

    let event: WSEvent = serde_json::from_str(json).unwrap();

    assert_eq!(event.pano, "abc123");
    assert_eq!(event.total_users, 123);
    assert_eq!(event.vote_counts.get(&1), Some(&10));
    assert_eq!(event.options.len(), 1);
    assert_eq!(event.end_time.timestamp(), 1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{Duration, timeout};

    #[tokio::test]
    #[ignore = "uses the network"]
    async fn smoke_test_real_ws() {
        let mut backend = WSBackend::new().await.unwrap();

        let event = timeout(Duration::from_secs(5), backend.next()).await;
        assert!(event.is_ok(), "Websocket did not respond in time");
    }
}

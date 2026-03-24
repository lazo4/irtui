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

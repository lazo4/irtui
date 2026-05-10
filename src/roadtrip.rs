use std::collections::HashMap;

use futures::StreamExt;
use ratatui::style::Color;
use serde::Deserialize;
use serde_aux::prelude::*;
use tracing::{Level, debug, error, info, instrument};
use wreq::{Client, WebSocket};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ChatEvent {
    pub author: String,
    pub content: String,
    pub color: Color,
}

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

/// And event we get from the neal.fun websocket
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
    // #[serde(deserialize_with = "deserialize_datetime_utc_from_milliseconds")]
    pub end_time: u64,
    pub chat_events: Vec<ChatEvent>,
    pub distance: f32,
}

/// Our current location, as per the websocket
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Location {
    pub road: String,
    pub neighborhood: Option<String>,
    pub state: String,
    pub county: Option<String>,
    pub country: String,
}

/// Websocket client that connects to the IRT websocket
pub struct WSBackend {
    socket: WebSocket,
}

impl WSBackend {
    /// Asynchronously connect to the IRT websocket
    ///
    /// ## Errors
    /// This fails if we can't connect to the websocket for some reason
    #[instrument(level = Level::DEBUG)]
    pub async fn new() -> Result<Self, anyhow::Error> {
        info!("Connecting to IRT websocket");
        let socket = Client::new()
            .websocket("wss://internet-roadtrip-listen-eqzms.ondigitalocean.app/")
            .send()
            .await?
            .into_websocket()
            .await?;

        info!("Connected to IRT websocket successfully");
        Ok(Self { socket })
    }
}

impl WSBackend {
    #[instrument(skip(self), level = Level::DEBUG)]
    pub async fn next(&mut self) -> Option<anyhow::Result<WSEvent>> {
        let maybe_message = self.socket.next().await?;
        let result = || -> anyhow::Result<WSEvent> {
            match maybe_message?.to_text() {
                Ok(text) => {
                    debug!("Received websocket message");
                    match serde_json::from_str::<WSEvent>(text) {
                        Ok(evt) => {
                            info!(
                                pano = %evt.pano,
                                location = ?evt.location,
                                users = evt.total_users,
                                "Received roadtrip event"
                            );
                            Ok(evt)
                        }
                        Err(e) => {
                            error!(error = %e, "Failed to parse websocket message as WSEvent");
                            Err(e.into())
                        }
                    }
                }
                Err(e) => {
                    error!(error = %e, "Failed to convert message to text");
                    Err(e.into())
                }
            }
        };

        Some(result())
    }
}

#[test]
fn test_ws_event_deserialization() {
    let json = r##"
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
        "endTime": 1000,
        "chatEvents": [
            {
                "type": "add",
                "id": "1499452156699213889",
                "author": "RibuRcul [head navigator]",
                "content": "The blue blobs are cities",
                "timestamp": 1777567652631,
                "color": "#88ff8a"
            }
        ],
        "distance":30000.00
    }
    "##;

    let event: WSEvent = serde_json::from_str(json).unwrap();

    assert_eq!(event.pano, "abc123");
    assert_eq!(event.total_users, 123);
    assert_eq!(event.vote_counts.get(&1), Some(&10));
    assert_eq!(event.options.len(), 1);
    assert_eq!(event.end_time, 1000);
    assert_eq!(event.distance, 30000.0);
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

        backend.next().await.unwrap().unwrap();
    }
}

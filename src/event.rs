use std::fmt::Debug;

use crossterm::event::{Event as CrosstermEvent, EventStream};
use futures::prelude::*;
use ratatui_image::protocol::Protocol;
use tokio::{sync::mpsc, task};
use tracing::warn;

use crate::roadtrip::{self, RoadtripEvent};

/// Representation of all possible events.
#[derive(Clone, Debug)]
pub enum Event {
    /// Roadtrip events
    /// Emitted each time the roadtrip moves
    RoadTrip(RoadtripEvent),
    /// Crossterm events.
    ///
    /// These events are emitted by the terminal.
    Crossterm(CrosstermEvent),
    /// Application events.
    ///
    /// Use this event to emit custom events that are specific to your application.
    App(AppEvent),
}

/// Application events.
///
/// You can extend this enum with your own custom events.
#[derive(Clone)]
pub enum AppEvent {
    /// A new frame has been rendered
    NewFrame(Protocol),
    /// Quit the application.
    Quit,
}

impl Debug for AppEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppEvent::NewFrame(proto) => {
                write!(f, "AppEvent::NewFrame(Protocol)")
            }
            AppEvent::Quit => write!(f, "AppEvent::Quit"),
        }
    }
}

/// Global event handler.
#[derive(Debug)]
pub struct EventHandler {
    /// Event sender channel.
    pub sender: mpsc::Sender<Event>,
    /// Event receiver channel.
    receiver: mpsc::Receiver<Event>,
}

impl EventHandler {
    /// Constructs a new instance of [`EventHandler`] and spawns a new threads to handle events.
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel(64); // Random, may change
        // Spawn a thread to handle crossterm events.
        {
            let sender = sender.clone();
            let mut event_stream = EventStream::new();
            task::spawn(async move {
                loop {
                    if let Some(Ok(event)) = event_stream.next().await
                        && sender.send(Event::Crossterm(event)).await.is_err()
                    {
                        break;
                    }
                }
            });
        }
        // Spawn a thread to handle roadtrip events.
        {
            let sender = sender.clone();
            task::spawn(async move {
                let mut backend = roadtrip::WSBackend::new()
                    .await
                    .expect("Failed to connect to roadtrip backend");
                while let Some(evt) = backend.next().await {
                    match evt {
                        Ok(evt) => {
                            if sender
                                .send(Event::RoadTrip(RoadtripEvent::WS(evt)))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(err) => warn!("Failed to recv from IRT WS: {err:#?}"),
                    }
                }
            });
        }
        Self { sender, receiver }
    }

    /// Receives an event from the sender.
    ///
    /// This function blocks until an event is received.
    ///
    /// # Errors
    ///
    /// This function returns an error if the sender channel is disconnected. This can happen if an
    /// error occurs in the event thread. In practice, this should not happen unless there is a
    /// problem with the underlying terminal.
    pub async fn next(&mut self) -> Option<Event> {
        self.receiver.recv().await
    }

    /// Queue an app event to be sent to the event receiver.
    ///
    /// This is useful for sending events to the event handler which will be processed by the next
    /// iteration of the application's event loop.
    pub async fn send(&mut self, app_event: AppEvent) {
        // Ignore the result as the reciever cannot be dropped while this struct still has a
        // reference to it
        let _ = self.sender.send(Event::App(app_event)).await;
    }
}

impl Default for EventHandler {
    fn default() -> Self {
        Self::new()
    }
}

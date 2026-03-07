use std::{fmt::Debug, time::Duration};

use crossterm::event::Event as CrosstermEvent;
use futures::prelude::*;
use ratatui_image::protocol::Protocol;
use tokio::{sync::mpsc, task};
use tracing::{debug, warn};

use crate::roadtrip::{self, RoadtripEvent};

const FPS: f64 = 15.0;

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

    /// Emitted at constant intervals
    ///
    /// Used to batch process events and render at a constant fps
    Tick,
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
            AppEvent::NewFrame(_) => {
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
    pub sender: mpsc::UnboundedSender<Event>,
    /// Event receiver channel.
    receiver: mpsc::UnboundedReceiver<Event>,
}

impl EventHandler {
    /// Constructs a new instance of [`EventHandler`] and spawns a new threads to handle events.
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel(); // Random, may change
        // Spawn a thread to handle crossterm events.
        {
            let sender = sender.clone();
            task::spawn(async move {
                let tick_rate = Duration::from_secs_f64(1.0 / FPS);
                let mut reader = crossterm::event::EventStream::new();
                let mut tick = tokio::time::interval(tick_rate);
                loop {
                    let tick_delay = tick.tick();
                    let crossterm_event = reader.next().fuse();
                    tokio::select! {
                      _ = sender.closed() => {
                        break;
                      }
                      _ = tick_delay => {
                        debug!("Sending tick event");
                        let _ = sender.send(Event::Tick);
                      }
                      Some(Ok(evt)) = crossterm_event => {
                        debug!("Sending crossterm event: {evt:?}");
                        let _ = sender.send(Event::Crossterm(evt));
                      }
                    };
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
        let _ = self.sender.send(Event::App(app_event));
    }
}

impl Default for EventHandler {
    fn default() -> Self {
        Self::new()
    }
}

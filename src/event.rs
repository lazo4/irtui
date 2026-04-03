use std::{fmt::Debug, time::Duration};

use crossterm::event::{Event as CrosstermEvent, EventStream};
use futures::prelude::*;
use ratatui_image::protocol::Protocol;
use tokio::{
    sync::mpsc::{self, UnboundedSender},
    task,
};
use tracing::{debug, warn};

use crate::roadtrip::{self, RoadtripEvent};

const FPS: f64 = 60.0;

/// An event that our application will process
#[derive(Clone, Debug)]
pub enum Event {
    /// Roadtrip events
    /// Emitted by the IRT WebSocket
    RoadTrip(RoadtripEvent),

    /// Crossterm events
    /// Emitted by the terminal
    Crossterm(CrosstermEvent),

    /// Internal events
    /// These are derived events emitted by the application
    App(AppEvent),

    /// Emitted at constant intervals
    /// Used to batch process events and render at a constant fps
    Tick,
}

/// Application events
#[derive(Clone)]
pub enum AppEvent {
    /// A streetview pano has been rendered and is ready to be displayed
    NewFrame(Protocol),

    /// The user wants to quit the app
    Quit,
}

/// Manual impl bc [`Protocol`] doesn't impl `Debug`
impl Debug for AppEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppEvent::NewFrame(_) => {
                write!(f, "AppEvent::NewFrame")
            }
            AppEvent::Quit => write!(f, "AppEvent::Quit"),
        }
    }
}

/// Spawn a task to forward crossterm and tick events
fn handle_crossterm_and_tick_evts(
    sender: UnboundedSender<Event>,
    mut crossterm_stream: impl Stream<Item = Result<CrosstermEvent, std::io::Error>>
    + Unpin
    + Send
    + 'static,
) {
    task::spawn(async move {
        let tick_rate = Duration::from_secs_f64(1.0 / FPS);
        let mut tick = tokio::time::interval(tick_rate);
        loop {
            let tick_delay = tick.tick();
            let crossterm_event = crossterm_stream.next().fuse();
            tokio::select! {
              () = sender.closed() => {
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

/// Global event handler, it is responsible for coordinating the various event sources and merging them into a
/// single stream for our app to handle.
#[derive(Debug)]
pub struct EventHandler {
    /// Send app derived events
    pub sender: mpsc::UnboundedSender<Event>,
    /// All events arrive here
    receiver: mpsc::UnboundedReceiver<Event>,
}

impl EventHandler {
    /// Constructs a new instance of [`EventHandler`] and spawns a new threads to handle events.
    #[must_use]
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel(); // Random, may change

        // Spawn a task to forward crossterm and tick events
        handle_crossterm_and_tick_evts(sender.clone(), EventStream::new());

        // Spawn a task to handle roadtrip events
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

    /// Asynchrounously wait for something to happen
    pub async fn next(&mut self) -> Option<Event> {
        self.receiver.recv().await
    }

    /// Queue an app event to be sent to the event receiver.
    ///
    /// This is useful for sending events to the event handler which will be processed by the next
    /// iteration of the application's event loop.
    pub fn send(&mut self, app_event: AppEvent) {
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

#[cfg(test)]
impl EventHandler {
    /// Returns an event handler that can be entirely controlled from the outside, for testing
    pub fn new_deterministic() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();

        Self { sender, receiver }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use pretty_assertions::assert_eq;
    use ratatui_image::protocol::halfblocks::Halfblocks;

    #[test]
    fn app_event_debug() {
        assert_eq!(format!("{:?}", AppEvent::Quit), "AppEvent::Quit");
        assert_eq!(
            format!(
                "{:?}",
                AppEvent::NewFrame(Protocol::Halfblocks(Halfblocks::default()))
            ),
            "AppEvent::NewFrame"
        );
    }

    #[tokio::test]
    #[ignore = "uses network and crossterm backend"]
    async fn test_event_handler() {
        let mut handler = EventHandler::default();
        let _ = handler.next().await;
        handler.send(AppEvent::Quit);
        // Wait for a WS Event and a Quit event, max five secs
        let before = Instant::now();
        let mut found_ws = false;
        let mut found_quit = false;
        while let Some(evt) = handler.next().await
            && (!found_ws || !found_quit)
            && before.elapsed().as_secs() < 5
        {
            if let Event::App(AppEvent::Quit) = evt {
                found_quit = true;
            }
            if let Event::RoadTrip(_) = evt {
                found_ws = true;
            }
        }

        assert!(found_quit);
        assert!(found_ws);
    }

    #[tokio::test]
    async fn test_tick_events() {
        use futures::stream;
        use tokio::time::{Duration, timeout};

        let dummy_stream = stream::pending();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();

        handle_crossterm_and_tick_evts(sender, dummy_stream);

        let evt = timeout(Duration::from_millis(50), receiver.recv())
            .await
            .expect("no event received in time")
            .unwrap();

        assert!(matches!(evt, Event::Tick));
    }

    #[tokio::test]
    async fn test_tick_input_fairness() {
        use futures::stream;
        use tokio::time::{Duration, sleep};

        // Stream that yields events, but not in a tight loop
        let input_stream = Box::pin(stream::unfold((), |_| async {
            sleep(Duration::from_millis(5)).await;
            Some((Ok(CrosstermEvent::FocusLost), ()))
        }));

        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();

        handle_crossterm_and_tick_evts(sender, input_stream);

        // Let the system run a bit
        sleep(Duration::from_millis(100)).await;

        let mut ticks = 0;
        let mut inputs = 0;

        while let Ok(evt) = receiver.try_recv() {
            match evt {
                Event::Tick => ticks += 1,
                Event::Crossterm(_) => inputs += 1,
                _ => {}
            }
        }

        // Both should have happened
        assert!(ticks > 0, "no ticks received");
        assert!(inputs > 0, "no input events received");
    }
}

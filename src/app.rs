use std::collections::HashMap;

use crate::{
    event::{AppEvent, Event, EventHandler},
    pano::spawn_rendering_task,
    roadtrip::{ChatEvent, Location, RoadtripEvent, VoteOption},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, prelude::*};
use ratatui_image::protocol::Protocol;
use tokio::sync::mpsc::Sender;
use tracing::{Level, debug, info, instrument, warn};
use wreq::Client;
use wreq_util::Emulation;

#[derive(Debug, Clone, PartialEq)]
pub enum PanoRequest {
    // Window was resized, re-render pano, using the cached tiles
    Resize(u16, u16),

    // New pano arrived, fetch tiles and render
    Render(String, f64), // panoid + heading
}

#[derive(Debug, Default)]
pub struct Hivechat {
    /// Wether the sidebar is hidden
    pub hidden: bool,

    /// The current messages the chat is displaying
    pub messages: Vec<ChatEvent>,

    /// Number of lines the user scrolled up, reset every time a new message arrives
    pub scroll_offset: u16,
}

/// Application
pub struct App {
    /// Is the application running?
    pub running: bool,

    /// Current panoid + heading
    pub current_pano: Option<(String, f64)>,

    /// Location info like town name, street name, etc...
    pub location: Option<Location>,

    /// Current rendered pano frame, to be rendered by ratatui-image
    pub cur_frame: Option<Protocol>,

    /// Event handler, coordinates and merges varous event sources
    pub events: EventHandler,

    // For sending pano render requests
    pub pano_tx: Sender<PanoRequest>,

    /// Number of users currently online
    pub users_online: u16,

    /// The current vote options available, aka, the arrows
    pub vote_options: Vec<VoteOption>,

    /// The vote counts for each option
    pub vote_counts: HashMap<i8, u16>,

    /// When the voting period ends
    pub vote_ends: Option<u64>,

    /// The current state of the Hivechat
    pub hivechat: Hivechat,
}

impl App {
    /// Return a default application which spawns the necessary hanlders and tasks
    ///
    /// # Errors
    /// This fails if setting up one of the tasks fails
    #[instrument(name = "App::init")]
    pub async fn with_default_term() -> anyhow::Result<Self> {
        // Spawn the default pano rendering task
        debug!("Creating crossterm EventHandler");
        let evt_handler = EventHandler::new();
        let (pano_tx, pano_rx) = tokio::sync::mpsc::channel::<PanoRequest>(10); // Idk why ten but why not?

        let evt_sender = evt_handler.sender.clone(); // So that rendering task can report back

        debug!("Spawning pano rendering task");
        spawn_rendering_task(pano_rx, evt_sender)?;

        debug!("Fetching chat messages");

        let response = Client::builder()
            .emulation(Emulation::Firefox139)
            .build()?
            .get("https://internet-roadtrip.neal.fun/chat")
            .header("Host", "internet-roadtrip.neal.fun")
            .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:146.0) Gecko/20100101 Firefox/139.0")
            .header("Accept", "*/*")
            .header("Accept-Language", "r,fr-FR;q=0.8,en-US;q=0.5,en;q=0.3")
            .header("Prefer", "safe")
            .header("Accept-Encoding", "gzip, deflate, br, zstd")
            .header("Referer", "https://neal.fun/")
            .header("Origin", "https://neal.fun")
            .header("Sec-GPC", "1")
            .header("Connection", "keep-alive")
            .header("Sec-Fetch-Dest", "empty")
            .header("Sec-Fetch-Mode", "cors")
            .header("Sec-Fetch-Site", "same-site")
            .header("Priority", "u=4")
            .header("TE", "trailers")
            .send()
            .await?
            .json()
            .await?;

        debug!("App initialized successfully");
        Ok(App::new(evt_handler, pano_tx, response))
    }

    /// Construct a new instance of [`App`], given an event source, pano sender and initial chat messages
    #[must_use]
    #[instrument(skip_all, level = Level::DEBUG)]
    pub fn new(
        evt_handler: EventHandler,
        pano_tx: Sender<PanoRequest>,
        messages: Vec<ChatEvent>,
    ) -> Self {
        Self {
            running: true,
            current_pano: None,
            location: None,
            pano_tx,
            events: evt_handler, // Spawn event handler thread
            cur_frame: None,
            users_online: 0,
            vote_options: Vec::new(),
            vote_counts: HashMap::new(),
            vote_ends: None,
            hivechat: Hivechat {
                messages,
                ..Hivechat::default()
            },
        }
    }

    /// Run the application's main loop.
    ///
    /// # Errors
    /// This fails if drawing to the terminal failed or if handling events failed
    #[instrument(skip_all, name = "App::run")]
    pub async fn run<B: Backend + Send + 'static>(
        mut self,
        mut terminal: Terminal<B>,
    ) -> anyhow::Result<()>
    where
        B::Error: Send + Sync,
    {
        info!("Starting app main loop");
        while self.running {
            terminal.draw(|frame| frame.render_widget(&mut self, frame.area()))?;
            self.handle_events().await?;
        }
        info!("App main loop ended");
        Ok(())
    }

    /// Handle incoming events, dispatching them to more specific handlers until it reaches a `Tick`
    ///
    /// # Errors
    /// This fails if the queue fails, probably impossible, but let's stay safe.
    pub async fn handle_events(&mut self) -> anyhow::Result<()> {
        let mut requested_size = None;
        while let Some(event) = self.events.next().await {
            debug!("Handling event: {event:?}");

            match event {
                Event::Crossterm(event) => match event {
                    crossterm::event::Event::Key(key_event)
                        if key_event.kind == crossterm::event::KeyEventKind::Press =>
                    {
                        self.handle_key_event(key_event);
                    }
                    crossterm::event::Event::Resize(width, height) => {
                        requested_size = Some((width, height));
                    }
                    _ => {}
                },
                Event::App(app_event) => match app_event {
                    AppEvent::NewFrame(proto) => self.cur_frame = Some(proto),
                    AppEvent::Quit => self.quit(),
                },
                Event::RoadTrip(roadtrip_event) => {
                    self.handle_roadtrip_event(roadtrip_event).await?;
                }
                // Tick is only a marker and shouldn't trigger specific behavior
                Event::Tick => {
                    // Avoid batches of resize events
                    if let Some((width, height)) = requested_size {
                        self.pano_tx
                            .send(PanoRequest::Resize(width, height))
                            .await?;
                    }
                    break;
                }
            }
        }
        Ok(())
    }

    /// Handle a roadtrip event
    ///
    /// # Errors
    /// Fails if the queue fails, which might turn out to be impossible, but we still return a `Result` just
    /// to be safe.
    #[instrument(skip(self), level = Level::DEBUG)]
    pub async fn handle_roadtrip_event(
        &mut self,
        roadtrip_event: RoadtripEvent,
    ) -> anyhow::Result<()> {
        match roadtrip_event {
            RoadtripEvent::WS(evt) => {
                debug!(
                    users_online = evt.total_users,
                    location = ?evt.location,
                    "Received roadtrip update"
                );

                self.users_online = evt.total_users;
                let panoid = evt.pano.clone();

                self.vote_counts = evt.vote_counts;
                self.vote_options = evt.options;
                self.vote_ends = Some(evt.end_time);

                if !evt.chat_events.is_empty() {
                    self.hivechat.scroll_offset = 0; // Go to bottom when new message arrives
                }

                self.hivechat.messages.extend(evt.chat_events);

                if self.current_pano != Some((evt.pano.clone(), evt.heading)) {
                    // Update current pano and trigger a render request.
                    info!(
                        panoid = %panoid,
                        heading = evt.heading,
                        "New panorama"
                    );
                    self.current_pano = Some((panoid.clone(), evt.heading));
                    self.location = Some(evt.location); // Imitate the website behavior
                    self.pano_tx
                        .send(PanoRequest::Render(panoid, evt.heading))
                        .await?;
                } else {
                    debug!("Panorama unchanged, skipping render");
                }
            }
        }
        Ok(())
    }

    /// Handles the key events and updates the state of [`App`].
    #[instrument(skip(self), level = Level::DEBUG)]
    pub fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                info!("Quit requested via keyboard");
                self.events.send(AppEvent::Quit);
            }
            KeyCode::Char('c' | 'C') if key_event.modifiers == KeyModifiers::CONTROL => {
                info!("Quit requested via Ctrl+C");
                self.events.send(AppEvent::Quit);
            }
            KeyCode::Char('c') => {
                debug!("Toggling HiveChat display");
                self.hivechat.hidden = !self.hivechat.hidden
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.hivechat.scroll_offset = self.hivechat.scroll_offset.saturating_sub(1); // Scroll down
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.hivechat.scroll_offset += 1; // Scroll up
            }
            _ => debug!(code = ?key_event.code, "Unhandled key event"),
        }
    }

    /// Set running to false to quit the application.
    pub fn quit(&mut self) {
        self.running = false;
    }
}

#[cfg(test)]
mod tests {
    use crate::roadtrip::WSEvent;

    use super::*;
    use chrono::Utc;
    use pretty_assertions::assert_eq;
    use tokio::sync::mpsc::{Receiver, UnboundedSender};

    fn new_test_app() -> (App, UnboundedSender<Event>, Receiver<PanoRequest>) {
        let evt_handler = EventHandler::new_deterministic();
        let (pano_tx, pano_rx) = tokio::sync::mpsc::channel::<PanoRequest>(10);
        let sender = evt_handler.sender.clone();
        (App::new(evt_handler, pano_tx, Vec::new()), sender, pano_rx)
    }

    #[tokio::test]
    #[ignore = "uses crossterm backend"]
    async fn test_app_default() {
        let app = App::with_default_term().await.unwrap();
        assert!(app.running);
        assert!(app.current_pano.is_none());
        assert!(app.location.is_none());
        assert!(app.cur_frame.is_none());
        assert_eq!(app.users_online, 0);
        assert!(app.vote_options.is_empty());
        assert!(app.vote_counts.is_empty());
        assert!(app.vote_ends.is_none());
    }

    /// This next series of tests verifies how the app reacts to external events
    #[tokio::test]
    async fn test_app_key_evts() {
        // Fake handler for testing
        let (mut app, sender, _) = new_test_app();

        // Send q event
        sender
            .send(Event::Crossterm(crossterm::event::Event::Key(
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            )))
            .unwrap();
        sender.send(Event::Tick).unwrap(); // To break the event loop and process the quit event
        app.handle_events().await.unwrap();

        sender.send(Event::Tick).unwrap();
        app.handle_events().await.unwrap();

        assert!(!app.running);
    }

    #[tokio::test]
    async fn test_app_resize() {
        let (mut app, sender, mut pano_rx) = new_test_app();

        // Send resize event
        sender
            .send(Event::Crossterm(crossterm::event::Event::Resize(100, 50)))
            .unwrap();
        sender
            .send(Event::Crossterm(crossterm::event::Event::Resize(50, 50)))
            .unwrap(); // Test batch resize
        sender.send(Event::Tick).unwrap(); // To break the event loop and process the resize event
        app.handle_events().await.unwrap();

        assert_eq!(pano_rx.recv().await.unwrap(), PanoRequest::Resize(50, 50));
    }

    #[tokio::test]
    async fn test_app_roadtrip_evt() {
        let (mut app, sender, mut pano_rx) = new_test_app();
        let end_time = Utc::now().timestamp() as u64 + 7;
        let event = Event::RoadTrip(RoadtripEvent::WS(WSEvent {
            pano: "tXVQoL_JtBEBbV7LYKW_2A".to_string(),
            heading: 90.0,
            location: Location {
                road: "Tremont St".to_string(),
                neighborhood: Some("Boston".to_string()),
                state: "Massachusetts".to_string(),
                county: Some("Suffolk".to_string()),
                country: "United States of America".to_string(),
            },
            total_users: 220,
            options: vec![
                VoteOption {
                    heading: 110.0,
                    pano: "CAoSFkNJSE0wb2dLRUlDQWdJQ0U5SVBWR1E.".to_string(),
                    description: Some("Local Business".to_string()),
                },
                VoteOption {
                    heading: 90.0,
                    pano: "LHa3O3Oo9bhVVJE1dtbsfg".to_string(),
                    description: Some("Tremont St".to_string()),
                },
            ],
            vote_counts: HashMap::from([(-1, 3), (-2, 2), (0, 8), (1, 3)]),
            end_time,
            chat_events: Vec::new(),
        }));
        sender.send(event).unwrap();
        sender.send(Event::Tick).unwrap(); // Break the loop

        app.handle_events().await.unwrap();

        assert_eq!(app.users_online, 220);
        assert_eq!(app.vote_options.len(), 2);
        assert_eq!(app.vote_counts.get(&-1), Some(&3));
        assert_eq!(app.vote_counts.get(&-2), Some(&2));
        assert_eq!(app.vote_ends, Some(end_time));
        assert_eq!(
            pano_rx.recv().await.unwrap(),
            PanoRequest::Render("tXVQoL_JtBEBbV7LYKW_2A".to_string(), 90.0)
        );
    }

    #[tokio::test]
    async fn test_app_run() {
        let (app, sender, _) = new_test_app();

        // Run the app in the background
        let backend = backend::TestBackend::new(80, 30);
        let terminal = Terminal::new(backend).unwrap();
        let app_handle = tokio::spawn(async move { app.run(terminal).await });
        sender.send(Event::App(AppEvent::Quit)).unwrap(); // To break the event loop immediately
        sender.send(Event::Tick).unwrap(); // To break the event loop immediately
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), app_handle).await;

        assert!(result.is_ok());
    }
}

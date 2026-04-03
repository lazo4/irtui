use std::collections::HashMap;

use crate::{
    event::{AppEvent, Event, EventHandler},
    pano::spawn_rendering_task,
    roadtrip::{Location, RoadtripEvent, VoteOption},
};

use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::DefaultTerminal;
use ratatui_image::protocol::Protocol;
use tokio::sync::mpsc::Sender;
use tracing::{debug, info};

pub enum PanoRequest {
    // Window was resized, re-render pano, using the cached tiles
    Resize(u16, u16),

    // New pano arrived, fetch tiles and render
    Render(String, f64), // panoid + heading
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
    pub vote_ends: Option<DateTime<Utc>>,
}

impl Default for App {
    fn default() -> Self {
        App::new(EventHandler::new())
    }
}

impl App {
    /// Constructs a new instance of [`App`].
    pub fn new(evt_handler: EventHandler) -> Self {
        // Spawn the pano rendering thread.
        let (pano_tx, pano_rx) = tokio::sync::mpsc::channel::<PanoRequest>(10); // Idk why ten but why not?

        let evt_sender = evt_handler.sender.clone(); // So that rendering task can report back

        debug!("Spawning pano rendering task");
        spawn_rendering_task(pano_rx, evt_sender);

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
        }
    }

    /// Run the application's main loop.
    pub async fn run(mut self, mut terminal: DefaultTerminal) -> anyhow::Result<()> {
        while self.running {
            debug!("Rendering");
            terminal.draw(|frame| frame.render_widget(&self, frame.area()))?;
            self.handle_events().await?;
        }
        Ok(())
    }

    pub async fn handle_events(&mut self) -> anyhow::Result<()> {
        let mut requested_size = None;
        while let Some(event) = self.events.next().await {
            info!("Handling event: {event:?}");

            match event {
                Event::Crossterm(event) => match event {
                    crossterm::event::Event::Key(key_event)
                        if key_event.kind == crossterm::event::KeyEventKind::Press =>
                    {
                        self.handle_key_event(key_event)?;
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
    pub async fn handle_roadtrip_event(
        &mut self,
        roadtrip_event: RoadtripEvent,
    ) -> anyhow::Result<()> {
        debug!("Recved roadtrip event {roadtrip_event:?}");
        match roadtrip_event {
            RoadtripEvent::WS(evt) => {
                self.users_online = evt.total_users;
                let panoid = evt.pano.clone();

                self.vote_counts = evt.vote_counts;
                self.vote_options = evt.options;
                self.vote_ends = Some(evt.end_time);

                if self.current_pano != Some((evt.pano, evt.heading)) {
                    // Update current pano and trigger a render request.
                    self.current_pano = Some((panoid.clone(), evt.heading));
                    self.location = Some(evt.location); // Imitate the website behavior
                    self.pano_tx
                        .send(PanoRequest::Render(panoid, evt.heading))
                        .await?;
                }
            }
        }
        Ok(())
    }

    /// Handles the key events and updates the state of [`App`].
    pub fn handle_key_event(&mut self, key_event: KeyEvent) -> anyhow::Result<()> {
        debug!("Recved key evt: {key_event:?}");
        match key_event.code {
            KeyCode::Esc | KeyCode::Char('q') => self.events.send(AppEvent::Quit),
            KeyCode::Char('c' | 'C') if key_event.modifiers == KeyModifiers::CONTROL => {
                self.events.send(AppEvent::Quit);
            }
            // Other handlers you could add here.
            _ => {}
        }
        Ok(())
    }

    /// Set running to false to quit the application.
    pub fn quit(&mut self) {
        self.running = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn test_app_default() {
        let app = App::default();
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
        let handler = EventHandler::new_deterministic();
        let sender = handler.sender.clone();
        let mut app = App::new(handler);

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
}

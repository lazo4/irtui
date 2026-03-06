use std::sync::Arc;

use crate::{
    event::{AppEvent, Event, EventHandler},
    pano::{get_pano_metadata_from_id, render_pano_from_metadata},
    roadtrip::{Location, RoadtripEvent},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{DefaultTerminal, layout::Rect};
use ratatui_image::{Resize, picker::Picker, protocol::Protocol};
use tokio::sync::mpsc::{Receiver, Sender};
use tracing::{debug, warn};

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

    pub current_pano: Option<(String, f64)>, // Current panoid + heading
    // Location info like town name, street name, etc...
    pub location: Option<Location>,

    pub cur_frame: Option<Protocol>,

    /// Event handler.
    pub events: EventHandler,
    // For sending pano render requests
    pub pano_tx: Sender<PanoRequest>,
}

impl Default for App {
    fn default() -> Self {
        App::new()
    }
}

impl App {
    /// Constructs a new instance of [`App`].
    pub fn new() -> Self {
        // Spawn the pano rendering thread.
        let (pano_tx, mut pano_rx) = tokio::sync::mpsc::channel::<PanoRequest>(10); // Idk why ten but why not?

        let evt_handler = EventHandler::new();

        let evt_sender = evt_handler.sender.clone();

        debug!("Spawning pano rendering task");
        tokio::task::spawn(async move {
            let picker = Picker::from_query_stdio().unwrap();

            let mut cur_size = crossterm::terminal::size().expect("Failed to query terminal size");
            let font_size = picker.font_size();

            while let Some(request) = pano_rx.recv().await {
                match request {
                    PanoRequest::Resize(width, height) => {
                        // Handle resize event
                        if cur_size == (width, height) {
                            continue; // No need to re-render if size didn't change
                        }
                        cur_size = (width, height);
                        // Convert characters to pixels
                        let width = width * font_size.0;
                        let height = height * font_size.1;
                    }
                    PanoRequest::Render(panoid, heading) => {
                        // Handle render event: fetch tiles and render into an image buffer.
                        let width = cur_size.0 * font_size.0;
                        let height = cur_size.1 * font_size.1;

                        // Render the pano
                        let meta = match get_pano_metadata_from_id(&panoid).await {
                            Ok(ok) => ok,
                            Err(err) => {
                                warn!("Failed to render pano {panoid}: {err:#?}");
                                continue;
                            }
                        };

                        let pano = match render_pano_from_metadata(
                            meta,
                            heading as f32,
                            width as u32,
                            height as u32,
                        )
                        .await
                        {
                            Ok(pano) => pano,
                            Err(err) => {
                                warn!("Failed to render pano {panoid}: {err:#?}");
                                continue;
                            }
                        };

                        let protocol = match picker.new_protocol(
                            pano.into(),
                            Rect::new(0, 0, width, height),
                            Resize::Crop(None),
                        ) {
                            Ok(proto) => proto,
                            Err(err) => {
                                warn!("Failed to create protocol for pano {panoid}: {err:?}");
                                continue;
                            }
                        };

                        evt_sender.send(Event::App(AppEvent::NewFrame(protocol))).await.unwrap();
                    }
                }
            }
        });

        Self {
            running: true,
            current_pano: None,
            location: None,
            pano_tx,
            events: evt_handler, // Spawn event handler thread
            cur_frame: None,
        }
    }

    /// Run the application's main loop.
    pub async fn run(mut self, mut terminal: DefaultTerminal) -> anyhow::Result<()> {
        while self.running {
            terminal.draw(|frame| frame.render_widget(&self, frame.area()))?;
            self.handle_events().await?;
        }
        Ok(())
    }

    pub async fn handle_events(&mut self) -> anyhow::Result<()> {
        if let Some(event) = self.events.next().await {
            match event {
                Event::Crossterm(event) => match event {
                    crossterm::event::Event::Key(key_event)
                        if key_event.kind == crossterm::event::KeyEventKind::Press =>
                    {
                        self.handle_key_event(key_event).await?
                    }
                    crossterm::event::Event::Resize(width, height) => {
                        self.pano_tx
                            .send(PanoRequest::Resize(width, height))
                            .await?;
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
                let panoid = evt.pano.clone();
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
    pub async fn handle_key_event(&mut self, key_event: KeyEvent) -> anyhow::Result<()> {
        debug!("Recved key evt: {key_event:?}");
        match key_event.code {
            KeyCode::Esc | KeyCode::Char('q') => self.events.send(AppEvent::Quit).await,
            KeyCode::Char('c' | 'C') if key_event.modifiers == KeyModifiers::CONTROL => {
                self.events.send(AppEvent::Quit).await;
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

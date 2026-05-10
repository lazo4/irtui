use std::cmp::{self, Reverse};

use chrono::Utc;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Offset, Rect, Size},
    style::{Color, Style, Stylize},
    symbols,
    text::{Line, Text},
    widgets::{Block, BorderType, Clear, LineGauge, Padding, Paragraph, Widget, Wrap},
};
use ratatui_image::Image;
use tracing::{Level, instrument};
use unicode_width::UnicodeWidthStr;

const WIDE_BREAK: u16 = 92;

use crate::app::{App, DistanceUnit, Hivechat, Odometer};

/// Compute the minimum width of a piece of text (kinda like css min-width I think)
fn compute_min_width(content: &str, wrap: bool) -> u16 {
    if wrap {
        // If we can wrap, calculate the longest word
        content
            .split_whitespace()
            .fold(0, |max, word| cmp::max(max, word.width())) as u16
    } else {
        content.width() as u16
    }
}

/// Calculate the border-box rectangle of a box, using shrink-to-fit, given the size available.
///
/// The function calculates the width and min-width of the content, applies padding and borders,
/// and determines the box size.
/// This function also allows for basic wrapping control, similar to css `white-space` set to `normal` or `nowrap`
///
/// Limitations: this function only works with text, and doesn't support other elts
///
/// Warning: the returned size may overflow the available rect, so always check for overflow
fn calculate_box_shrink_to_fit(
    content: &str,
    can_wrap: bool,
    padding: Padding,
    borders: bool,
    available_width: u16,
) -> Size {
    let content_width = content.width() as u16;
    let min_content_width = compute_min_width(content, can_wrap);
    let box_width = content_width + padding.left + padding.right + if borders { 2 } else { 0 };
    let min_box_width =
        min_content_width + padding.left + padding.right + if borders { 2 } else { 0 };
    let final_width = available_width.clamp(min_box_width, box_width);

    // Now calculate wrapping based on our newly aquired width
    let content_height = if can_wrap {
        // Calculate wrapping
        textwrap::wrap(
            content,
            (final_width - padding.left - padding.right - if borders { 2 } else { 0 }) as usize,
        )
        .len() as u16
    } else {
        // If wrapping isn't allowed, height is straightforward
        1
    };

    let final_height = content_height + padding.top + padding.bottom + if borders { 2 } else { 0 };

    Size::new(final_width, final_height)
}

impl App {
    /// Render the current streetview frame as the background of the UI
    fn render_frame(&self, area: Rect, buf: &mut Buffer) {
        // Display the current streetview frame
        if let Some(proto) = &self.cur_frame {
            let image = Image::new(proto);
            image.render(area, buf);
        }
    }

    /// Render the the town and street boxes, approximating the website layout
    fn render_location(&self, area: Rect, buf: &mut Buffer) {
        if let Some(location) = &self.location {
            let content = format!(
                "{}, {}",
                location
                    .neighborhood
                    .as_ref()
                    .or(location.county.as_ref())
                    .unwrap_or(&String::new()),
                location.country
            );

            // Compute the properties of the town box
            let padding = if area.width >= WIDE_BREAK {
                Padding::uniform(1)
            } else {
                Padding::ZERO
            };
            let box_size = calculate_box_shrink_to_fit(
                &content,
                area.width >= WIDE_BREAK,
                padding,
                true,
                area.width / 2, // Box should take up half of screen width
            );

            // Center the rect inside the screen
            let town_rect = Rect::new(
                0,
                if area.width >= WIDE_BREAK { 0 } else { 4 }, // Move it down on narrow displays
                area.width,
                box_size.height,
            )
            .centered_horizontally(Constraint::Length(box_size.width));

            App::render_town_box(town_rect, buf, &content, padding, area.width >= WIDE_BREAK);

            // Compute the properties of the street box
            let box_size = calculate_box_shrink_to_fit(
                &location.road,
                true, // As per the website, the street name always wraps
                Padding::ZERO,
                true,
                area.width / 2,
            );

            let street_rect = Rect::new(0, town_rect.bottom(), area.width, box_size.height)
                .centered_horizontally(Constraint::Length(box_size.width));

            App::render_street_box(street_rect, buf, &location.road);
        }
    }

    /// Render the top green box with the town name
    fn render_town_box(
        area: Rect,
        buf: &mut Buffer,
        content: &str,
        padding: Padding,
        can_wrap: bool,
    ) {
        let mut town_name = Paragraph::new(content)
            .style(
                Style::default()
                    .bg(Color::Rgb(0, 132, 48))
                    .fg(Color::Rgb(255, 255, 255)),
            )
            .centered()
            .bold()
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .padding(padding),
            );

        if can_wrap {
            town_name = town_name.wrap(Wrap { trim: true });
        }

        Clear.render(area, buf);
        town_name.render(area, buf);
    }

    /// Render the white box with the street name in the given `area`
    fn render_street_box(area: Rect, buf: &mut Buffer, road: &str) {
        let street_name = Paragraph::new(road)
            .style(
                Style::default()
                    .bg(Color::Rgb(255, 255, 255))
                    .fg(Color::Black),
            )
            .centered()
            .wrap(Wrap { trim: false })
            .block(Block::bordered().border_type(BorderType::Rounded));
        Clear.render(area, buf);
        street_name.render(area, buf);
    }

    /// Render the box with the current drivers count in the top right corner of the rect
    fn render_drivers_online(&self, area: Rect, buf: &mut Buffer) {
        let content: Line = vec![
            "● ".fg(Color::Rgb(255, 0, 0)),
            format!("{} drivers online", self.users_online).black(),
        ]
        .into();
        let content_width = content.width() as u16 + 4;
        let content_rect = Rect::new(
            area.width.saturating_sub(content_width),
            0,
            content_width,
            3,
        )
        .clamp(area);
        Clear.render(content_rect, buf);
        let drivers_online = Paragraph::new(content)
            .style(Style::default().bg(Color::Rgb(255, 242, 2)))
            .centered()
            .block(Block::bordered().border_type(BorderType::Rounded).black())
            .bold();
        drivers_online.render(content_rect, buf);
    }

    /// Render the vote counts box, below the drivers count
    /// TODO: make this code cleaner, and shorter, for now were gonna just:
    #[allow(clippy::too_many_lines)]
    fn render_vote_counts(&self, area: Rect, buf: &mut Buffer) {
        // Render the vote counts box
        if let Some(end_time) = self.vote_ends
            && let Some((_, heading)) = &self.current_pano
        {
            let secs_until = end_time.saturating_sub(Utc::now().timestamp_millis() as u64) / 1000;
            let picking_in = if secs_until > 0 {
                format!(
                    "Picking {}in {secs_until} seconds...",
                    if area.width >= WIDE_BREAK {
                        "option "
                    } else {
                        ""
                    }
                )
            } else {
                format!(
                    "Picking{}...",
                    if area.width >= WIDE_BREAK {
                        " option"
                    } else {
                        ""
                    }
                )
            };
            let init_text_height = if secs_until > 0 { 2 } else { 1 };

            let mut vote_counts: Vec<_> = self.vote_counts.iter().collect();
            vote_counts.sort_by_key(|(idx, count)| Reverse((**count, **idx)));
            vote_counts.truncate(4);

            let total = vote_counts
                .iter()
                .map(|(_, count)| **count)
                .sum::<u16>()
                .max(1);

            let content_width = if area.width >= WIDE_BREAK { 21 } else { 18 };
            let content_rect = Rect::new(
                area.width.saturating_sub(content_width),
                4,
                content_width,
                init_text_height + 1 + 2 + vote_counts.len() as u16 * 2,
            )
            .clamp(area);

            let block = Block::bordered()
                .border_type(BorderType::Rounded)
                .bg(Color::Rgb(255, 255, 255))
                .black();
            let inner_rect = block.inner(content_rect);

            Clear.render(content_rect, buf);
            block.render(content_rect, buf);

            Paragraph::new(picking_in)
                .black()
                .wrap(Wrap { trim: false })
                .render(inner_rect, buf);

            let counts_rect = inner_rect.offset(Offset::new(0, init_text_height as i32 + 1));

            for (offset, (idx, count)) in vote_counts.iter().enumerate() {
                let mut emoji = match idx {
                    -1 => "⏩",
                    -2 => "📢",
                    0.. => {
                        let aro_heading = self.vote_options[**idx as usize].heading;
                        let heading_diff = aro_heading - heading;
                        match heading_diff.round() as i16 {
                            -102..-67 => "⬅️",
                            -67..-22 => "↖️",
                            -22..23 => "⬆️",
                            23..68 => "↗️",
                            68..102 => "➡️",
                            _ => "",
                        }
                    }
                    _ => "",
                }
                .to_string();
                let ratio = **count as f64 / total as f64;
                emoji.extend(std::iter::repeat_n(" ", 2 - emoji.width()));

                let text = format!("{emoji} {count} votes");
                let percentage = format!("{}%", (ratio * 100.0).round());

                let text = Paragraph::new(text).left_aligned().black();
                let percent = Paragraph::new(percentage).right_aligned().black();

                let gauge = LineGauge::default()
                    .filled_symbol(symbols::line::THICK_HORIZONTAL)
                    .unfilled_symbol(" ")
                    .black()
                    .label(Line::default())
                    .ratio(ratio);

                let text_rect = Rect::new(
                    counts_rect.x,
                    counts_rect.y + (offset as u16 * 2),
                    counts_rect.width,
                    1,
                )
                .clamp(area);
                let gauge_rect = Rect::new(
                    text_rect.x + 2,
                    text_rect.y + 1,
                    text_rect.width - 2,
                    text_rect.height,
                )
                .clamp(area);

                text.render(text_rect, buf);
                percent.render(text_rect, buf);
                gauge.render(gauge_rect, buf);
            }
        }
    }
}

impl Widget for &mut App {
    /// Render the whole UI.
    #[instrument(skip(self, buf), level = Level::TRACE)]
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.render_frame(area, buf);
        self.hivechat.render(area, buf);
        self.render_location(area, buf);
        self.render_drivers_online(area, buf);
        self.render_vote_counts(area, buf);
        self.odometer.render(area, buf);
    }
}

impl Widget for &Odometer {
    fn render(self, area: Rect, buf: &mut Buffer)
    where
        Self: Sized,
    {
        let distance = match self.unit {
            DistanceUnit::Miles => self.distance,
            DistanceUnit::Kilometers => self.distance * 1.609_344,
        } as u64;

        let distance_str = if distance == 0 {
            "00000".to_string()
        } else {
            format!("{distance:03}")
        };

        let line1 =
            ("▄".to_string() + &"▄▄".repeat(distance_str.len()) + "▄▄").fg(Color::Rgb(0, 0, 0));
        let line2 = ("▌".to_string()
            + distance_str
                .chars()
                .fold(String::new(), |s, c| s + &c.to_string() + "│")
                .trim_end_matches('│')
            + "▐")
            .fg(Color::Rgb(0, 0, 0))
            .bg(Color::Rgb(255, 255, 255))
            + match self.unit {
                DistanceUnit::Miles => "Mi",
                DistanceUnit::Kilometers => "km",
            }
            .fg(Color::Rgb(255, 255, 255))
            .bg(Color::Rgb(0, 0, 0));
        let line3 =
            ("▀".to_string() + &"▀▀".repeat(distance_str.len()) + "▀▀").fg(Color::Rgb(0, 0, 0));

        let width = line1.width() as u16;
        let text = Paragraph::new(vec![line1.into(), line2, line3.into()]);

        text.render(
            Rect::new(
                area.width.saturating_sub(width),
                area.height.saturating_sub(3),
                width,
                3,
            ),
            buf,
        );
    }
}

impl Widget for &mut Hivechat {
    fn render(self, area: Rect, buf: &mut Buffer)
    where
        Self: Sized,
    {
        let width = 20;
        let height = 10;
        let content_rect = Rect::new(area.width.saturating_sub(width), 0, width, area.height)
            .centered_vertically(Constraint::Length(height))
            .clamp(area);
        if !self.hidden {
            let text: Text = self
                .messages
                .iter()
                .map(|msg| {
                    Line::from(vec![
                        msg.author.as_str().fg(msg.color).bold(),
                        " ".into(),
                        msg.content.as_str().black(),
                    ])
                })
                .collect();

            let messages = Paragraph::new(text)
                .bg(Color::Rgb(255, 255, 255))
                .wrap(Wrap { trim: false });
            let num_lines = messages.line_count(content_rect.width) as u16;

            let max_scroll = num_lines.saturating_sub(content_rect.height);
            self.scroll_offset = self.scroll_offset.min(max_scroll);

            let y_offset = num_lines
                .saturating_sub(content_rect.height)
                .saturating_sub(self.scroll_offset);

            let messages = messages.scroll((y_offset, 0));

            Clear.render(content_rect, buf);
            messages.render(content_rect, buf);
        }

        let can_draw = if self.hidden {
            area.width >= 2
        } else {
            content_rect.x >= 2
        };
        if can_draw {
            buf.set_string(
                if self.hidden {
                    area.width - 2
                } else {
                    content_rect.x - 2
                },
                content_rect.y,
                "💬",
                Style::default().bg(Color::Rgb(255, 255, 255)),
            );
        }
    }
}

/// UI tests, they are long, they are clunky, this is normal
#[cfg(test)]
mod tests {

    use std::collections::HashMap;

    use crate::{
        event::EventHandler,
        roadtrip::{ChatEvent, Location},
    };

    use pretty_assertions::assert_eq;

    use super::*;

    /// check if the area and content (raw text) of two buffers are the same
    pub fn assert_buffer_eq(buffer: &ratatui::buffer::Buffer, expected: &ratatui::buffer::Buffer) {
        // if this is false, the test passes
        if buffer.area() != expected.area()
            || !buffer
                .content()
                .iter()
                .zip(expected.content().iter())
                .all(|(a, b)| a.symbol() == b.symbol())
        {
            // otherwise, let's "assert" that they are the same, simply so that `pretty_assertions::assert_eq` will print the diff
            pretty_assertions::assert_eq!(buffer, expected);
        }
    }

    #[test]
    fn test_full_render() {
        let (tx, _) = tokio::sync::mpsc::channel(100);
        let mut app = App::new(EventHandler::new_deterministic(), tx, Vec::new());

        let area = Rect::new(0, 0, 100, 50);
        let mut buf = Buffer::empty(area);
        app.render(area, &mut buf);

        // Narrow
        let area = Rect::new(0, 0, 20, 70);
        let mut buf = Buffer::empty(area);
        app.render(area, &mut buf);

        // Very small
        let area = Rect::new(0, 0, 5, 5);
        let mut buf = Buffer::empty(area);
        app.render(area, &mut buf);

        // Very wide
        let area = Rect::new(0, 0, 300, 50);
        let mut buf = Buffer::empty(area);
        app.render(area, &mut buf);
    }

    #[test]
    fn test_render_drivers_online() {
        let (tx, _) = tokio::sync::mpsc::channel(100);
        let mut app = App::new(EventHandler::new_deterministic(), tx, Vec::new());
        app.users_online = 100;

        let area = Rect::new(0, 0, 30, 5);
        let mut buf = Buffer::empty(area);
        app.render_drivers_online(area, &mut buf);

        assert_buffer_eq(
            &buf,
            &Buffer::with_lines(vec![
                "      ╭──────────────────────╮",
                "      │ ● 100 drivers online │",
                "      ╰──────────────────────╯",
                "                              ",
                "                              ",
            ]),
        );

        // Text is clipped if the rect is too narrow
        let area = Rect::new(0, 0, 10, 5);
        let mut buf = Buffer::empty(area);
        app.render_drivers_online(area, &mut buf);

        assert_buffer_eq(
            &buf,
            &Buffer::with_lines(vec![
                "╭────────╮",
                "│● 100 dr│",
                "╰────────╯",
                "          ",
                "          ",
            ]),
        );
    }

    #[test]
    fn test_render_location_narrow() {
        let (tx, _) = tokio::sync::mpsc::channel(100);
        let mut app = App::new(EventHandler::new_deterministic(), tx, Vec::new());

        app.location = Some(Location {
            neighborhood: Some("Town of East Hampton".to_string()), // Wide text for testing
            country: "United States of America".to_string(),
            road: "Main Street".to_string(),
            county: Some("Suffolk County".to_string()), // Random
            state: "New York".to_string(),              // Random
        });

        let area = Rect::new(0, 0, 10, 11); // Narrow, this is practical, not realistic
        let mut buf = Buffer::empty(area);
        app.render_location(area, &mut buf);

        assert_buffer_eq(
            &buf,
            &Buffer::with_lines(vec![
                "          ",
                "          ",
                "          ",
                "          ",
                "╭────────╮",
                "│Town of │",
                "╰────────╯",
                " ╭──────╮ ",
                " │ Main │ ",
                " │Street│ ", // At least text truncation is tested
                " ╰──────╯ ",
            ]),
        );
    }

    #[test]
    fn test_render_location_wide() {
        let (tx, _) = tokio::sync::mpsc::channel(100);
        let mut app = App::new(EventHandler::new_deterministic(), tx, Vec::new());

        app.location = Some(Location {
            neighborhood: Some(
                "Town of East Hampton, East Historical Village District, Bla bla bla bla bla bla"
                    .to_string(),
            ), // Wide text for testing
            country: "United States of America".to_string(),
            road: "Very very loooong street street street street name name".to_string(),
            county: Some("Suffolk County".to_string()), // Random
            state: "New York".to_string(),              // Random
        });

        let area = Rect::new(0, 0, WIDE_BREAK, 11); // Test wide layout
        let mut buf = Buffer::empty(area);
        app.render_location(area, &mut buf);

        assert_buffer_eq(
            &buf,
            &Buffer::with_lines(vec![
                // Rustfmt i will kill you if you format this differently
                "                       ╭────────────────────────────────────────────╮                       ",
                "                       │                                            │                       ",
                "                       │    Town of East Hampton, East Historical   │                       ",
                "                       │ Village District, Bla bla bla bla bla bla, │                       ",
                "                       │          United States of America          │                       ",
                "                       │                                            │                       ",
                "                       ╰────────────────────────────────────────────╯                       ",
                "                       ╭────────────────────────────────────────────╮                       ",
                "                       │   Very very loooong street street street   │                       ",
                "                       │              street name name              │                       ",
                "                       ╰────────────────────────────────────────────╯                       ",
            ]),
        );
    }

    #[test]
    fn test_vote_counts() {
        let (tx, _) = tokio::sync::mpsc::channel(100);
        let mut app = App::new(EventHandler::new_deterministic(), tx, Vec::new());

        app.vote_ends = Some(Utc::now().timestamp_millis() as u64 + 7200);
        app.current_pano = Some((String::new(), 90.0));
        app.vote_options = vec![
            crate::roadtrip::VoteOption {
                heading: 90.0,
                pano: String::new(),
                description: None,
            },
            crate::roadtrip::VoteOption {
                heading: 170.0,
                pano: String::new(),
                description: None,
            },
        ];
        // Seek five, forward, 10 and right 15
        app.vote_counts = HashMap::from([(-1, 5), (0, 10), (1, 15)]);

        let area = Rect::new(0, 0, 30, 13);
        let mut buf = Buffer::empty(area);
        app.render_vote_counts(area, &mut buf);

        assert_buffer_eq(
            &buf,
            &Buffer::with_lines(vec![
                "                              ",
                "                              ",
                "            ╭────────────────╮",
                "            │Picking in 7    │",
                "            │seconds...      │",
                "            │                │",
                "            │➡️ 15 votes  50%│", // hidden by multi-width symbols: [(14, " ")]
                "            │   ━━━━━━       │",
                "            │⬆️ 10 votes  33%│", // hidden by multi-width symbols: [(14, " ")]
                "            │   ━━━━         │",
                "            │⏩ 5 votes   17%│", // hidden by multi-width symbols: [(14, " ")]
                "            │   ━━           │",
                "            ╰────────────────╯",
            ]),
        );
    }

    #[test]
    fn test_hivechat() {
        let mut chat = Hivechat {
            hidden: false,
            scroll_offset: 0,
            messages: vec![
                crate::roadtrip::ChatEvent {
                    author: "Test".to_string(),
                    content: "testing testing testing".to_string(),
                    color: Color::Rgb(255, 0, 0),
                },
                ChatEvent {
                    author: "Test2".to_string(),
                    content: "testing2 testing2 testing2".to_string(),
                    color: Color::Rgb(0, 255, 0),
                },
            ],
        };
        let area = Rect::new(0, 0, 22, 10);
        let mut buf = Buffer::empty(area);

        chat.render(area, &mut buf);
        assert_buffer_eq(
            &buf,
            &Buffer::with_lines(vec![
                "💬Test testing testing", // hidden by multi-width symbols: [(1, " ")]
                "  testing             ",
                "  Test2 testing2      ",
                "  testing2 testing2   ",
                "                      ",
                "                      ",
                "                      ",
                "                      ",
                "                      ",
                "                      ",
            ]),
        );
    }

    #[test]
    fn test_odometer_render() {
        let odometer = Odometer {
            distance: 33000.0,
            unit: DistanceUnit::Miles,
        };

        let area = Rect::new(0, 0, 15, 3);
        let mut buf = Buffer::empty(area);
        odometer.render(area, &mut buf);
        assert_buffer_eq(
            &buf,
            &Buffer::with_lines(vec![
                "  ▄▄▄▄▄▄▄▄▄▄▄▄▄",
                "  ▌3│3│0│0│0▐Mi",
                "  ▀▀▀▀▀▀▀▀▀▀▀▀▀",
            ]),
        );

        let odometer = Odometer {
            distance: 10000.0,
            unit: DistanceUnit::Kilometers,
        };

        let area = Rect::new(0, 0, 15, 3);
        let mut buf = Buffer::empty(area);
        odometer.render(area, &mut buf);
        assert_buffer_eq(
            &buf,
            &Buffer::with_lines(vec![
                "  ▄▄▄▄▄▄▄▄▄▄▄▄▄",
                "  ▌1│6│0│9│3▐km",
                "  ▀▀▀▀▀▀▀▀▀▀▀▀▀",
            ]),
        );
    }

    #[test]
    fn test_min_width() {
        assert_eq!(compute_min_width("123 1234", true), 4);
        assert_eq!(
            compute_min_width("Town of East Hampton, United States of America", true),
            8
        );
    }

    #[test]
    fn test_min_width_no_wrap() {
        assert_eq!(compute_min_width("123 1234", false), 8);
    }

    #[test]
    fn test_min_width_empty() {
        assert_eq!(compute_min_width("", true), 0);
    }

    #[test]
    fn test_min_width_single_word() {
        assert_eq!(compute_min_width("hello", true), 5);
    }

    #[test]
    fn test_min_width_unicode() {
        // "é" = width 1, "界" = width 2
        assert_eq!(compute_min_width("éé é", true), 2);
        assert_eq!(compute_min_width("hello 世界", true), 5);
    }
}

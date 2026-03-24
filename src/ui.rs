use std::cmp::{self, Reverse, min};

use chrono::Utc;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Offset, Rect, Size},
    style::{Color, Style, Stylize},
    symbols,
    text::Line,
    widgets::{Block, BorderType, Clear, LineGauge, Padding, Paragraph, Widget, Wrap},
};
use ratatui_image::Image;
use unicode_width::UnicodeWidthStr;

const WIDE_BREAK: u16 = 92;

use crate::app::App;

// Compute min_width of a piece of text (kinda like css min-width I think)
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

impl Widget for &App {
    /// Render the whole UI.
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Display the current streetview frame
        if let Some(proto) = &self.cur_frame {
            let image = Image::new(proto);
            image.render(area, buf);
        }

        if let Some(location) = &self.location {
            let content = format!("{}, {}", location.neighborhood, location.country);
            // Do this like in CSS, content, padding, border, using border-box like algorithm
            // First compute width and min-width, then height, once wrapping is sorted out

            let content_width = content.width() as u16;
            let min_content_width = compute_min_width(&content, area.width > WIDE_BREAK);

            let mut padding = if area.width > WIDE_BREAK {
                Padding::uniform(1)
            } else {
                Padding::ZERO
            };

            let preferred_box_width = content_width + padding.right + padding.left + 2; // 2 is for border
            let min_box_width = min_content_width + padding.left + padding.right + 2;

            let box_width = cmp::min(cmp::max(min_box_width, area.width / 2), preferred_box_width);

            // Now for height

            // If we didn't wrap
            let content_height = if preferred_box_width <= box_width {
                1
            } else {
                // We wrapped, for now assume a height of 2
                // TODO: fix this
                2
            };

            let box_height = content_height + padding.top + padding.bottom + 2; // 2 border that is

            let box_rect = if area.width > WIDE_BREAK {
                // Wide layout
                Rect::new(0, 0, area.width, box_height)
                    .centered_horizontally(Constraint::Max(box_width)) // Width is shrink to fit
            } else {
                // Narrow layout
                Rect::new(0, 4, area.width, box_height)
                    .centered_horizontally(Constraint::Max(box_width)) // Width is shrink to fit
            };

            let mut town_name = Paragraph::new(content)
                .style(Style::default().bg(Color::Rgb(0, 132, 48)).fg(Color::White))
                .centered()
                .bold()
                .block(
                    Block::bordered()
                        .border_type(BorderType::Rounded)
                        .padding(padding),
                );

            if area.width > WIDE_BREAK {
                town_name = town_name.wrap(Wrap { trim: true });
            }

            Clear.render(box_rect, buf);

            town_name.render(box_rect, buf);

            let content_rect = Rect::new(0, box_rect.bottom(), area.width, 3)
                .centered_horizontally(Constraint::Max(location.road.len() as u16 + 2));

            let street_name = Paragraph::new(location.road.clone())
                .style(Style::default().bg(Color::White).fg(Color::Black))
                .centered()
                .block(Block::bordered().border_type(BorderType::Rounded));
            street_name.render(content_rect, buf);
        }

        let content: Line = vec![
            "● ".red(),
            format!("{} drivers online", self.users_online).black(),
        ]
        .into();
        let content_width = content.width() as u16 + 4;
        let content_rect = Rect::new(area.width - content_width, 0, content_width, 3);
        Clear.render(content_rect, buf);
        let drivers_online = Paragraph::new(content)
            .style(Style::default().bg(Color::Rgb(255, 242, 2)))
            .centered()
            .block(Block::bordered().border_type(BorderType::Rounded).black())
            .bold();
        drivers_online.render(content_rect, buf);

        // Render the vote counts box
        if let Some(end_time) = self.vote_ends
            && let Some((_, heading)) = &self.current_pano
        {
            let secs_until = end_time.signed_duration_since(Utc::now()).num_seconds();
            let picking_in = if secs_until > 0 {
                format!("Picking option in {secs_until} seconds...")
            } else {
                "Picking option...".to_string()
            };
            let content_width = 21;
            let content_rect = Rect::new(area.width - content_width, 4, content_width, 18);
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

            let mut vote_counts: Vec<_> = self.vote_counts.iter().collect();
            vote_counts.sort_by_key(|(idx, count)| Reverse((**count, **idx)));
            let total = vote_counts
                .iter()
                .map(|(_, count)| **count)
                .sum::<u16>()
                .max(1);

            let counts_rect = inner_rect.offset(Offset::new(0, 3));

            for (offset, (idx, count)) in vote_counts.iter().take(4).enumerate() {
                let mut emoji = match idx {
                    -1 => "⏭",
                    -2 => "📢",
                    0.. => {
                        let aro_heading = self.vote_options[**idx as usize].heading;
                        let heading_diff = aro_heading - heading;
                        match heading_diff.round() as i16 {
                            -102..-67 => "⬅", // TODO: Better emoji support
                            -67..-22 => "↖",
                            -22..23 => "⬆",
                            23..68 => "↗",
                            68..102 => "➡︎",
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
                );
                let gauge_rect = Rect::new(
                    text_rect.x + 2,
                    text_rect.y + 1,
                    text_rect.width - 2,
                    text_rect.height,
                );

                text.render(text_rect, buf);
                percent.render(text_rect, buf);
                gauge.render(gauge_rect, buf);
            }
        }
    }
}

#[test]
fn test_min_width() {
    assert_eq!(compute_min_width("123 1234", true), 4);
    assert_eq!(
        compute_min_width("Town of East Hampton, United States of America", true),
        8
    );
}

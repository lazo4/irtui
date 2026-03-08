use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style, Stylize},
    text::Line,
    widgets::{Block, BorderType, Clear, Padding, Paragraph, Widget},
};
use ratatui_image::Image;

use crate::app::App;

impl Widget for &App {
    /// Renders the user interface widgets.
    ///
    // This is where you add new widgets.
    // See the following resources:
    // - https://docs.rs/ratatui/latest/ratatui/widgets/index.html
    // - https://github.com/ratatui/ratatui/tree/master/examples
    fn render(self, area: Rect, buf: &mut Buffer) {
        if let Some(proto) = &self.cur_frame {
            let image = Image::new(proto);
            image.render(area, buf);
        }

        if let Some(location) = &self.location {
            let content = format!("{}, {}", location.neighborhood, location.country);
            let content_width = content.len() as u16;
            let content_rect = if area.width > 92 {
                Rect::new(0, 0, area.width, 5)
                    .centered_horizontally(Constraint::Max(content_width + 4))
            } else {
                Rect::new(0, 3, area.width, 3)
                    .centered_horizontally(Constraint::Max(content_width + 4))
            };

            let town_name = Paragraph::new(content)
                .style(Style::default().bg(Color::Rgb(0, 132, 48)).fg(Color::White))
                .centered()
                .bold()
                .block(
                    Block::bordered()
                        .border_type(BorderType::Rounded)
                        .padding(Padding::vertical(if area.width > 92 { 1 } else { 0 })),
                );

            Clear.render(content_rect, buf);

            town_name.render(content_rect, buf);

            let content_rect = Rect::new(0, content_rect.bottom(), area.width, 3)
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
    }
}

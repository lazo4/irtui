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
        if let Some(location) = &self.location
            && let Some(proto) = &self.cur_frame
        {
            let image = Image::new(proto);
            image.render(area, buf);

            let layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(5),
                    Constraint::Length(3),
                    Constraint::Min(0),
                ])
                .split(area);

            let content = format!("{}, {}", location.neighborhood, location.country);
            let content_width = content.len() as u16;

            let town_name = Paragraph::new(content)
                .style(Style::default().bg(Color::Rgb(0, 132, 48)).fg(Color::White))
                .centered()
                .bold()
                .block(
                    Block::bordered()
                        .border_type(BorderType::Rounded)
                        .padding(Padding::vertical(1)),
                );

            Clear.render(
                layout[0].centered_horizontally(Constraint::Max(content_width + 4)),
                buf,
            );

            town_name.render(
                layout[0].centered_horizontally(Constraint::Max(content_width + 4)),
                buf,
            );

            let street_name = Paragraph::new(location.road.clone())
                .style(Style::default().bg(Color::White).fg(Color::Black))
                .centered()
                .block(Block::bordered().border_type(BorderType::Rounded));
            street_name.render(
                layout[1].centered_horizontally(Constraint::Max(location.road.len() as u16 + 2)),
                buf,
            );

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
}

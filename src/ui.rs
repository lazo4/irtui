use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style, Stylize},
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
        }
    }
}

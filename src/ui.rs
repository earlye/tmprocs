use crate::app::{App, ProcStatus};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    let scroll_offset = draw_proc_list(frame, app, chunks[0]);
    draw_help_bar(frame, chunks[1]);

    // Position the cursor at the selected row so iTerm's cursor-row highlight
    // stays in sync with the selection highlight.
    let visible_row = app.selected.saturating_sub(scroll_offset) as u16;
    let cursor_y = (chunks[0].y + 1 + visible_row) // +1 for top border
        .min(chunks[0].y + chunks[0].height.saturating_sub(2));
    frame.set_cursor_position((0, cursor_y));
}

fn draw_proc_list(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) -> usize {
    let items: Vec<ListItem> = app
        .procs
        .iter()
        .map(|p| {
            let (status_char, status_color) = match p.status {
                ProcStatus::Running => ("●", Color::Green),
                ProcStatus::Dead => ("○", Color::Red),
            };
            let shown_marker = if p.is_shown { "▶ " } else { "  " };
            let line = Line::from(vec![
                Span::raw(shown_marker),
                Span::styled(status_char, Style::default().fg(status_color)),
                Span::raw(" "),
                Span::raw(p.name.clone()),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("processes"))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

    let mut state = ListState::default();
    state.select(Some(app.selected));

    frame.render_stateful_widget(list, area, &mut state);
    state.offset()
}

fn draw_help_bar(frame: &mut Frame, area: ratatui::layout::Rect) {
    let help = Paragraph::new("↑/k up  ↓/j down  Enter focus  s start  x kill  q quit")
        .style(Style::default().fg(Color::DarkGray));
    frame.render_widget(help, area);
}

//! Drawing the TUI. Layout and colour only — no decisions live here.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use spotless_core::SafetyTier;

use super::{App, Tab};
use crate::ui;

/// Colour by safety tier, matching the words the CLI prints.
fn tier_style(tier: SafetyTier) -> Style {
    match tier {
        SafetyTier::Safe => Style::new().fg(Color::Green),
        SafetyTier::Caution => Style::new().fg(Color::Yellow),
        SafetyTier::Expert => Style::new().fg(Color::Red),
    }
}

fn tier_word(tier: SafetyTier) -> &'static str {
    match tier {
        SafetyTier::Safe => "safe",
        SafetyTier::Caution => "caution",
        SafetyTier::Expert => "expert",
    }
}

pub fn draw(frame: &mut Frame, app: &App) {
    let areas = Layout::vertical([
        Constraint::Length(1), // title
        Constraint::Length(1), // tabs
        Constraint::Min(1),    // body
        Constraint::Length(1), // status
    ])
    .split(frame.area());

    title(frame, areas[0], app);
    tabs(frame, areas[1], app);
    body(frame, areas[2], app);
    status(frame, areas[3], app);

    if let Some(confirm) = &app.confirm {
        popup(frame, &confirm.question);
    }
}

fn title(frame: &mut Frame, area: Rect, app: &App) {
    let (label, bytes) = match app.tab {
        Tab::Scan | Tab::Dev => ("selected", app.selected_bytes()),
        Tab::Apps => ("highlighted", app.selected_bytes()),
        Tab::Trash => ("in the Trash", app.selected_bytes()),
    };
    let line = Line::from(vec![
        Span::styled(" Spotless ", Style::new().bold().fg(Color::Cyan)),
        Span::styled(
            format!("{} {}", ui::bytes(bytes), label),
            Style::new().fg(Color::Gray),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn tabs(frame: &mut Frame, area: Rect, app: &App) {
    let titles: Vec<Line> = Tab::ALL
        .iter()
        .map(|t| Line::from(format!(" {} ", t.title())))
        .collect();
    let widget = Tabs::new(titles)
        .select(app.tab.index())
        .divider("")
        .style(Style::new().fg(Color::DarkGray))
        .highlight_style(Style::new().bold().fg(Color::Black).bg(Color::Cyan));
    frame.render_widget(widget, area);
}

fn body(frame: &mut Frame, area: Rect, app: &App) {
    match app.tab {
        Tab::Scan => list(frame, area, app, scan_rows(app, area.width)),
        Tab::Dev => list(frame, area, app, dev_rows(app, area.width)),
        Tab::Apps => list(frame, area, app, app_rows(app, area.width)),
        Tab::Trash => trash(frame, area, app),
    }
}

/// The shared list chrome: a bordered, scrolling list with the cursor row lit.
fn list(frame: &mut Frame, area: Rect, app: &App, items: Vec<ListItem>) {
    if items.is_empty() {
        let message = if app.busy.is_some() {
            "Measuring…"
        } else {
            "Nothing here. Press r to scan again."
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(Style::new().fg(Color::DarkGray))
                .block(Block::default().borders(Borders::ALL)),
            area,
        );
        return;
    }

    let widget = List::new(items)
        .block(Block::default().borders(Borders::ALL))
        .highlight_style(Style::new().bold().bg(Color::Rgb(40, 44, 52)))
        .highlight_symbol("");

    // The state is built per frame from the cursor; ratatui works out an offset
    // that keeps the selected row on screen, which is all the scrolling this
    // needs.
    let mut state = ListState::default().with_selected(Some(app.cursor()));
    frame.render_stateful_widget(widget, area, &mut state);
}

fn tick(selected: bool) -> Span<'static> {
    if selected {
        Span::styled(" [x] ", Style::new().fg(Color::Cyan))
    } else {
        Span::styled(" [ ] ", Style::new().fg(Color::DarkGray))
    }
}

fn scan_rows(app: &App, width: u16) -> Vec<ListItem<'_>> {
    // The name column takes whatever the terminal has left after the fixed
    // ones, so a narrow window loses description text rather than alignment.
    let name_width = (width as usize).saturating_sub(30).max(12);
    app.scan
        .iter()
        .map(|row| {
            let target = &row.item.target;
            ListItem::new(Line::from(vec![
                tick(row.selected),
                Span::styled(
                    ui::rpad(&ui::bytes(row.item.total_bytes), 9),
                    Style::new().bold(),
                ),
                Span::raw("  "),
                Span::styled(
                    ui::lpad(tier_word(target.safety), 8),
                    tier_style(target.safety),
                ),
                Span::raw(" "),
                Span::raw(ui::truncate_path(&target.name, name_width)),
            ]))
        })
        .collect()
}

fn dev_rows(app: &App, width: u16) -> Vec<ListItem<'_>> {
    let path_width = (width as usize).saturating_sub(32).max(12);
    let home = spotless_core::paths::home_dir();
    app.dev
        .iter()
        .map(|row| {
            let artifact = &row.item;
            // Shown relative to home: every row would otherwise start with the
            // same fifteen characters.
            let shown = home
                .as_ref()
                .and_then(|h| artifact.path.strip_prefix(h).ok())
                .unwrap_or(&artifact.path)
                .display()
                .to_string();
            ListItem::new(Line::from(vec![
                tick(row.selected),
                Span::styled(
                    ui::rpad(&ui::bytes(artifact.size_bytes), 9),
                    Style::new().bold(),
                ),
                Span::raw("  "),
                Span::styled(ui::lpad(&artifact.tool, 10), Style::new().fg(Color::Cyan)),
                Span::raw(" "),
                Span::raw(ui::truncate_path(&shown, path_width)),
            ]))
        })
        .collect()
}

fn app_rows(app: &App, width: u16) -> Vec<ListItem<'_>> {
    let name_width = (width as usize).saturating_sub(20).max(12);
    app.apps
        .iter()
        .map(|installed| {
            ListItem::new(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    ui::rpad(&ui::bytes(installed.size_bytes.unwrap_or(0)), 9),
                    Style::new().bold(),
                ),
                Span::raw("  "),
                Span::raw(ui::truncate_path(&installed.name, name_width)),
            ]))
        })
        .collect()
}

fn trash(frame: &mut Frame, area: Rect, app: &App) {
    let text = match app.trash.as_ref() {
        // Not measured yet. Saying "empty" here would be a lie the user has no
        // way to tell from the truth.
        None => Text::styled("Measuring...", Style::new().fg(Color::DarkGray)),
        Some(trash) if trash.items == 0 => Text::from("The Trash is empty."),
        Some(trash) => Text::from(vec![
            Line::from(vec![
                Span::styled(ui::bytes(trash.bytes), Style::new().bold()),
                Span::raw(format!(" in {}.", ui::count(trash.items, "item", "items"))),
            ]),
            Line::raw(""),
            Line::styled(
                "Everything Spotless removes goes here first, so the space only",
                Style::new().fg(Color::DarkGray),
            ),
            Line::styled(
                "comes back once the Trash is emptied. Press e to empty it —",
                Style::new().fg(Color::DarkGray),
            ),
            Line::styled("that part is permanent.", Style::new().fg(Color::DarkGray)),
        ]),
    };
    frame.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: true }).block(
            Block::default()
                .borders(Borders::ALL)
                .padding(ratatui::widgets::Padding::new(1, 1, 1, 1)),
        ),
        area,
    );
}

fn status(frame: &mut Frame, area: Rect, app: &App) {
    let line = match &app.busy {
        Some(progress) => Line::from(vec![
            Span::styled(" ⣾ ", Style::new().fg(Color::Cyan)),
            Span::styled(progress.clone(), Style::new().fg(Color::Gray)),
        ]),
        None => Line::from(vec![
            Span::styled(format!(" {} ", app.status), Style::new().fg(Color::Gray)),
            Span::styled(keys(app.tab), Style::new().fg(Color::DarkGray)),
        ]),
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn keys(tab: Tab) -> &'static str {
    match tab {
        Tab::Scan | Tab::Dev => "· a all · r rescan · tab switch · q quit",
        Tab::Apps | Tab::Trash => "· r refresh · tab switch · q quit",
    }
}

/// The confirmation dialog. Nothing destructive happens without passing here.
fn popup(frame: &mut Frame, question: &str) {
    let area = centered(frame.area(), 60, 7);
    frame.render_widget(Clear, area);
    let text = Text::from(vec![
        Line::raw(""),
        Line::styled(question.to_string(), Style::new().bold()),
        Line::raw(""),
        Line::styled(
            "y to confirm · any other key to cancel",
            Style::new().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::new().fg(Color::Yellow))
                    .title(" Confirm "),
            ),
        area,
    );
}

/// A `width` × `height` box in the middle of `area`, clamped to fit.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

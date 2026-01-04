use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::backend::CrosstermBackend;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};
use std::{error::Error, io};

const PROMPT: &str = "> ";
const VIEWPORT_HEIGHT: u16 = 6;

#[derive(Default)]
struct Model {
    input: String,
}

enum Msg {
    Input(char),
    Paste(String),
    Backspace,
    Submit,
    Quit,
}

enum Cmd {
    Submit(String),
}

pub fn run(handler: fn(&str) -> String) -> Result<(), Box<dyn Error>> {
    enable_raw_mode()?;
    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(VIEWPORT_HEIGHT),
        },
    )?;

    let mut model = Model::default();
    let res = run_app(&mut terminal, &mut model, handler);

    disable_raw_mode()?;
    terminal.show_cursor()?;

    res?;
    Ok(())
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    model: &mut Model,
    handler: fn(&str) -> String,
) -> io::Result<()> {
    loop {
        terminal.draw(|f| view(f, model))?;

        if let Some(msg) = read_msg()? {
            if matches!(msg, Msg::Quit) {
                return Ok(());
            }

            if let Some(cmd) = update(model, msg) {
                run_cmd(terminal, cmd, handler)?;
            }
        }
    }
}

fn view(f: &mut Frame, model: &Model) {
    let area = f.area();
    let wrapped = wrap_prompted_lines(PROMPT, &model.input, area.width);
    let line_count = wrapped.lines.len().max(1);
    let scroll = line_count.saturating_sub(area.height as usize);
    let input = Paragraph::new(wrapped.lines).scroll((scroll as u16, 0));
    f.render_widget(input, area);

    let cursor_line = line_count.saturating_sub(1).saturating_sub(scroll);
    let cursor_y = area.y + cursor_line as u16;
    let cursor_x = area.x + PROMPT.len() as u16 + wrapped.last_len as u16;
    if cursor_x < area.x + area.width && cursor_y < area.y + area.height {
        f.set_cursor_position((cursor_x, cursor_y));
    }
}

fn read_msg() -> io::Result<Option<Msg>> {
    match event::read()? {
        Event::Key(key) => {
            if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                return Ok(None);
            }
            let msg = match key.code {
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Msg::Quit,
                KeyCode::Enter => Msg::Submit,
                KeyCode::Char(c) => {
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        || key.modifiers.contains(KeyModifiers::ALT)
                    {
                        return Ok(None);
                    }
                    Msg::Input(c)
                }
                KeyCode::Backspace => Msg::Backspace,
                _ => return Ok(None),
            };
            Ok(Some(msg))
        }
        Event::Paste(text) => Ok(Some(Msg::Paste(text))),
        _ => Ok(None),
    }
}

fn update(model: &mut Model, msg: Msg) -> Option<Cmd> {
    match msg {
        Msg::Input(ch) => {
            push_input_char(&mut model.input, ch);
            None
        }
        Msg::Paste(text) => {
            push_input_str(&mut model.input, &text);
            None
        }
        Msg::Backspace => {
            model.input.pop();
            None
        }
        Msg::Submit => {
            let payload = std::mem::take(&mut model.input);
            if payload.trim().is_empty() {
                None
            } else {
                Some(Cmd::Submit(payload))
            }
        }
        Msg::Quit => None,
    }
}

fn run_cmd(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    cmd: Cmd,
    handler: fn(&str) -> String,
) -> io::Result<()> {
    match cmd {
        Cmd::Submit(payload) => {
            let width = terminal.size()?.width;
            let mut lines = wrap_prompted_lines(PROMPT, &payload, width).lines;

            let response = handler(&payload);
            if !response.trim().is_empty() {
                lines.extend(wrap_plain_lines(&response, width));
            }

            let height = lines.len().max(1) as u16;
            terminal.insert_before(height, move |buf| {
                Paragraph::new(lines).render(buf.area, buf);
            })?;
        }
    }
    Ok(())
}

struct WrappedLines {
    lines: Vec<Line<'static>>,
    last_len: usize,
}

fn wrap_prompted_lines(prefix: &str, text: &str, width: u16) -> WrappedLines {
    let width = width.max(1) as usize;
    let prefix_len = prefix.len();
    let content_width = width.saturating_sub(prefix_len).max(1);

    let content_lines = wrap_text(text, content_width);
    let last_len = content_lines.last().map(|line| line.len()).unwrap_or(0);
    let mut lines = Vec::with_capacity(content_lines.len());
    let indent = " ".repeat(prefix_len);

    for (idx, line) in content_lines.iter().enumerate() {
        let head = if idx == 0 { prefix } else { &indent };
        lines.push(Line::from(format!("{head}{line}")));
    }

    WrappedLines { lines, last_len }
}

fn wrap_plain_lines(text: &str, width: u16) -> Vec<Line<'static>> {
    let width = width.max(1) as usize;
    wrap_text(text, width).into_iter().map(Line::from).collect()
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for raw_line in text.split('\n') {
        lines.extend(split_to_width(raw_line, width));
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn split_to_width(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_len = 0;

    for ch in text.chars() {
        current.push(ch);
        current_len += 1;
        if current_len >= width {
            lines.push(current);
            current = String::new();
            current_len = 0;
        }
    }

    if lines.is_empty() {
        lines.push(current);
    } else if !current.is_empty() {
        lines.push(current);
    }

    lines
}

fn push_input_char(input: &mut String, ch: char) {
    let normalized = match ch {
        '\n' | '\r' => ' ',
        _ => ch,
    };
    input.push(normalized);
}

fn push_input_str(input: &mut String, text: &str) {
    for ch in text.chars() {
        push_input_char(input, ch);
    }
}

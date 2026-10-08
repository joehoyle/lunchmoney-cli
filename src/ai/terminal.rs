//! Shared terminal presentation for account and transaction chat.
use anyhow::{Result, bail};
use futures_util::StreamExt;
use genai::{
    Client,
    chat::{ChatOptions, ChatRequest, ChatStreamEvent, MessageContent, StopReason},
};
use std::{
    env,
    future::Future,
    io::{self, IsTerminal, Write},
    time::{Duration, Instant},
};
use unicode_width::UnicodeWidthChar;

pub(crate) const PRESENTATION: &str = "You are chatting in a terminal. Write clear, concise replies with short paragraphs and simple bullets when helpful. Use Markdown bold, headings and code sparingly; these are rendered for the terminal. Put each bullet on its own line, with a blank line before a list. Avoid wide tables. Keep tool narration brief; progress is shown locally.";

fn animated() -> bool {
    io::stdout().is_terminal() && env::var("TERM").as_deref() != Ok("dumb")
}

fn style(text: &str, code: &str) -> String {
    if animated() && env::var_os("NO_COLOR").is_none() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}

pub(crate) fn banner(title: &str, model: &str, help: &str) {
    println!("\n{}  {}", style(title, "1;36"), style(model, "2"));
    println!("{}\n", style(help, "2"));
}

pub(crate) fn prompt(text: &str) -> io::Result<()> {
    print!("{}", style(text, "1;36"));
    io::stdout().flush()
}

pub(crate) struct Reply {
    pub content: MessageContent,
    pub reasoning_content: Option<String>,
}

struct Display {
    label: String,
    started: Instant,
    frame: usize,
    waiting: bool,
    speaking: bool,
    markdown: super::markdown::Markdown,
}

impl Display {
    fn new(label: &str) -> Self {
        let width = terminal_size::terminal_size()
            .map(|(width, _)| usize::from(width.0))
            .unwrap_or(80);
        let limit = width.saturating_sub(16).clamp(1, 64);
        let mut used = 0;
        let label: String = label
            .chars()
            .filter(|c| !c.is_control())
            .take_while(|c| {
                used += c.width().unwrap_or(0);
                used <= limit
            })
            .collect();
        let mut display = Self {
            label,
            started: Instant::now(),
            frame: 0,
            waiting: false,
            speaking: false,
            markdown: super::markdown::Markdown::new(
                animated() && env::var_os("NO_COLOR").is_none(),
            ),
        };
        if !animated() {
            println!("{}…", display.label);
        }
        display.tick();
        display
    }

    fn tick(&mut self) {
        if animated() && !self.speaking {
            let frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let elapsed = self.started.elapsed().as_secs();
            let suffix = if elapsed >= 2 {
                format!(" · {elapsed}s")
            } else {
                String::new()
            };
            print!(
                "\r\x1b[2K{}",
                style(
                    &format!(
                        "{} {}…{suffix}",
                        frames[self.frame % frames.len()],
                        self.label
                    ),
                    "2"
                )
            );
            self.frame += 1;
            self.waiting = true;
            let _ = io::stdout().flush();
        }
    }

    fn clear(&mut self) {
        if self.waiting {
            print!("\r\x1b[2K");
            self.waiting = false;
        }
    }

    fn text(&mut self, text: &str) -> io::Result<()> {
        let text = self.markdown.push(text, false);
        if text.is_empty() {
            return Ok(());
        }
        if !self.speaking {
            self.clear();
            print!("{}", style("AI: ", "1;35"));
            self.speaking = true;
        }
        print!("{text}");
        io::stdout().flush()
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        self.clear();
        let tail = self.markdown.push("", true);
        if !tail.is_empty() {
            if !self.speaking {
                print!("{}", style("AI: ", "1;35"));
                self.speaking = true;
            }
            print!("{tail}");
        }
        if self.speaking {
            println!("\n");
        }
        let _ = io::stdout().flush();
    }
}

/// Print text as it arrives; only return completed content for tool execution.
/// Reasoning and signatures stay in history, never in terminal output.
pub(crate) async fn reply(client: &Client, model: &str, request: ChatRequest) -> Result<Reply> {
    let options = ChatOptions::default()
        .with_capture_content(true)
        .with_capture_tool_calls(true)
        .with_capture_reasoning_content(true);
    let mut display = Display::new("Thinking");
    let mut ticks = tokio::time::interval(Duration::from_millis(90));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let opening = client.exec_chat_stream(model, request, Some(&options));
    tokio::pin!(opening);
    let mut response = loop {
        tokio::select! {
            result = &mut opening => break result.map_err(super::client::chat_error)?,
            _ = ticks.tick() => display.tick(),
        }
    };
    loop {
        tokio::select! {
            event = response.stream.next() => match event {
                Some(Ok(ChatStreamEvent::Chunk(chunk))) => display.text(&chunk.content)?,
                Some(Ok(ChatStreamEvent::End(end))) => {
                    if !matches!(end.captured_stop_reason, Some(StopReason::Completed(_) | StopReason::ToolCall(_) | StopReason::StopSequence(_))) {
                        bail!("AI reply was interrupted or incomplete; no tool actions were run");
                    }
                    return Ok(Reply {
                        content: end.captured_content.unwrap_or_default(),
                        reasoning_content: end.captured_reasoning_content,
                    });
                }
                Some(Ok(_)) => {},
                Some(Err(error)) => return Err(super::client::chat_error(error)),
                None => bail!("AI stream ended before completion; no tool actions were run"),
            },
            _ = ticks.tick(), if !display.speaking => display.tick(),
        }
    }
}

/// Animate a pending tool without ever displaying its arguments or result payload.
pub(crate) async fn activity<T>(label: &str, task: impl Future<Output = Result<T>>) -> Result<T> {
    let mut display = Display::new(label);
    let mut ticks = tokio::time::interval(Duration::from_millis(90));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tokio::pin!(task);
    let result = loop {
        tokio::select! {
            result = &mut task => break result,
            _ = ticks.tick() => display.tick(),
        }
    };
    display.clear();
    if animated() && result.is_ok() {
        println!("{}", style(&format!("↳ {}", display.label), "2"));
    }
    result
}

//! Following a job of the server to its end. On a terminal it is a line
//! with a spinner and the last lines of the job's output beneath it, which
//! give way to one line with the result. Elsewhere the output goes to
//! standard error as it comes, and the result to standard output.

use super::client::Client;
use super::fail::{Fail, Result};
use super::look::Role;
use super::out::{self, Style};
use ratatui::{
    Terminal, TerminalOptions, Viewport,
    backend::CrosstermBackend,
    style::{Modifier, Style as Look},
    text::{Line, Span},
    widgets::Paragraph,
};
use serde_json::{Value, json};
use std::io::Write;
use std::time::{Duration, Instant};

/// How many lines of output are shown under the spinner.
const TAIL: usize = 5;
/// How many lines of a failed job are printed when it ends.
const SHOWN_ON_FAILURE: usize = 25;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// How a job ended.
pub struct Ended {
    pub code: i32,
    /// What the server says of it now.
    pub job: Value,
    /// The last lines of its output, without their colours.
    pub tail: Vec<String>,
}

/// The lines of output seen, the last of which may not be finished.
#[derive(Default)]
struct Lines {
    done: Vec<String>,
    open: String,
}

impl Lines {
    fn take(&mut self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        for ch in text.chars() {
            match ch {
                '\n' => self.done.push(std::mem::take(&mut self.open)),
                // A line written over: what follows replaces it.
                '\r' => self.open.clear(),
                ch => self.open.push(ch),
            }
        }
        // Enough to show the end of a failed job.
        let over = self.done.len().saturating_sub(SHOWN_ON_FAILURE * 4);
        self.done.drain(..over);
    }

    fn last(&self, count: usize) -> Vec<String> {
        let mut all: Vec<String> = self.done.iter().map(|line| out::strip_ansi(line)).collect();
        let open = out::strip_ansi(&self.open);
        if !open.trim().is_empty() {
            all.push(open);
        }
        all.retain(|line| !line.trim().is_empty());
        let from = all.len().saturating_sub(count);
        all[from..].to_vec()
    }
}

fn elapsed(since: Instant) -> String {
    let seconds = since.elapsed().as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        _ => format!("{}m {:02}s", seconds / 60, seconds % 60),
    }
}

/// The progress drawn on the terminal.
struct Inline {
    terminal: Terminal<CrosstermBackend<std::io::Stderr>>,
}

impl Inline {
    fn start() -> Option<Inline> {
        let terminal = Terminal::with_options(
            CrosstermBackend::new(std::io::stderr()),
            TerminalOptions {
                viewport: Viewport::Inline(TAIL as u16 + 1),
            },
        )
        .ok()?;
        Some(Inline { terminal })
    }

    fn draw(&mut self, title: &str, since: Instant, lines: &Lines) {
        let frame = (since.elapsed().as_millis() / 100) as usize % SPINNER.len();
        let tail = lines.last(TAIL);
        let _ = self.terminal.draw(|f| {
            let width = f.area().width as usize;
            let mut shown = vec![Line::from(vec![
                Span::styled(SPINNER[frame], out::look(Role::Accent)),
                Span::raw(" "),
                Span::styled(title.to_owned(), Look::new().add_modifier(Modifier::BOLD)),
                Span::styled(format!("  {}", elapsed(since)), out::look(Role::Muted)),
            ])];
            for line in &tail {
                let cut: String = line.chars().take(width.saturating_sub(4)).collect();
                shown.push(Line::styled(format!("  {cut}"), out::look(Role::Muted)));
            }
            f.render_widget(Paragraph::new(shown), f.area());
        });
    }

    fn end(mut self) {
        let _ = self.terminal.clear();
        out::to_line_of(&mut self.terminal);
    }
}

/// What following a job came to before it ended.
enum Watched {
    Ended,
    /// Ctrl+C: stop watching.
    Interrupted,
}

/// Ask whether Ctrl+C stops a job the server keeps, or only the watching.
fn stop_or_keep(id: &str) -> bool {
    let question = rookery_setup::flow::Question {
        step: rookery_setup::fields::Step::Run,
        label: "Stop the job, or keep it running?".into(),
        help: format!("It goes on where the server runs; rookery job stop {id} stops it later."),
        flag: String::new(),
        asked: rookery_setup::flow::Asked::Choice {
            options: vec![
                rookery_setup::flow::Choice {
                    key: "keep".into(),
                    label: "keep it running".into(),
                    help: format!("Stop watching. rookery job logs {id} -f follows it again."),
                },
                rookery_setup::flow::Choice {
                    key: "stop".into(),
                    label: "stop the job".into(),
                    help: "Its programs end, and its record says who stopped it.".into(),
                },
            ],
            default: 0,
        },
        required: false,
    };
    matches!(
        super::ask::ask(&question),
        Ok(rookery_setup::flow::Answer::Choice(1))
    )
}

/// Follow the job `id` to its end, shown as `title`. With `own` the job's
/// output is what the command is for, such as an action of a host, and goes
/// to standard output whole. Ctrl+C asks whether to stop a job that a
/// server keeps, at a terminal; elsewhere it stops watching.
pub fn job(client: &Client, id: &str, title: &str, own: bool) -> Result<Ended> {
    let shown = out::get();
    let colour = shown.colour.1;
    let mut inline = match shown.terminal.1 && !shown.quiet && !own {
        true => Inline::start(),
        false => None,
    };
    let since = Instant::now();
    let mut lines = Lines::default();
    let mut after = 0i64;
    let mut asked_to_stop = false;
    let stopped = loop {
        let mut stream = client.stream("jobs.follow", json!({"id": id, "after": after}))?;
        let watched = client.wait(async {
            let mut tick = tokio::time::interval(Duration::from_millis(100));
            let mut buffer = vec![];
            loop {
                tokio::select! {
                    line = stream.line(&mut buffer) => match line.map_err(Fail::from)? {
                        Some(item) if item.get("seq").is_some() => {
                            after = item["seq"].as_i64().unwrap_or(after);
                            let mut text = item["text"].as_str().unwrap_or("").to_owned();
                            text.push('\n');
                            lines.take(text.as_bytes());
                            if own {
                                let mut stdout = std::io::stdout().lock();
                                let _ = stdout.write_all(text.as_bytes());
                                let _ = stdout.flush();
                            } else if inline.is_none() && !shown.quiet {
                                let text = match colour { true => text, false => out::strip_ansi(&text) };
                                let mut stderr = std::io::stderr().lock();
                                let _ = stderr.write_all(text.as_bytes());
                                let _ = stderr.flush();
                            }
                        }
                        // The last line says how it ended.
                        Some(_) | None => return Ok::<_, Fail>(Watched::Ended),
                    },
                    _ = tick.tick() => {
                        if let Some(inline) = &mut inline {
                            inline.draw(title, since, &lines);
                        }
                    }
                    _ = tokio::signal::ctrl_c() => return Ok(Watched::Interrupted),
                }
            }
        })?;
        match watched {
            Watched::Ended => break false,
            Watched::Interrupted if asked_to_stop || !shown.ask => break true,
            Watched::Interrupted => {
                // The question needs the terminal's lines to itself.
                if let Some(drawn) = inline.take() {
                    drawn.end();
                }
                if !stop_or_keep(id) {
                    break true;
                }
                asked_to_stop = true;
                client.call("jobs.cancel", json!({"id": id}))?;
                out::note(format!("Asked {id} to stop. Ctrl+C again stops watching"));
                if !shown.quiet && !own {
                    inline = Inline::start();
                }
            }
        }
    };
    let drew = inline.is_some();
    if let Some(inline) = inline {
        inline.end();
    }
    if stopped {
        let message = format!(
            "Stopped watching. The job goes on where the server runs: rookery job logs {id} -f"
        );
        return Err(Fail {
            status: 130,
            code: "stopped".into(),
            message,
        });
    }
    let job = client
        .call("jobs.get", json!({"id": id}))
        .unwrap_or(Value::Null);
    let code = match job["state"].as_str() {
        Some("succeeded") => 0,
        Some("cancelled") => 130,
        _ => super::jobs::exit_of(&job)
            .filter(|code| *code != 0)
            .unwrap_or(1),
    };
    let took = elapsed(since);
    if !shown.quiet {
        match code {
            0 => eprintln!(
                "{} {title} {}",
                out::styled_err("✓", Style::Good),
                out::styled_err(&format!("done in {took}"), Style::Dim)
            ),
            code => {
                // Output drawn inline is gone from the screen; its end is shown again.
                if drew {
                    let mut stderr = std::io::stderr().lock();
                    for line in lines.done.iter().rev().take(SHOWN_ON_FAILURE).rev() {
                        let line = match colour {
                            true => line.clone(),
                            false => out::strip_ansi(line),
                        };
                        let _ = writeln!(stderr, "  {line}");
                    }
                }
                let said = match job["state"].as_str() {
                    Some("cancelled") => {
                        format!("stopped after {took}. Its log: rookery job logs {id}")
                    }
                    _ => match job["error"]["message"].as_str() {
                        Some(why) if super::jobs::exit_of(&job).is_none() => {
                            format!("failed after {took}: {why}. Its log: rookery job logs {id}")
                        }
                        _ => format!(
                            "failed with exit status {code} after {took}. Its log: rookery job logs {id}"
                        ),
                    },
                };
                eprintln!(
                    "{} {title} {}",
                    out::styled_err("✗", Style::Bad),
                    out::styled_err(&said, Style::Dim)
                );
            }
        }
    }
    Ok(Ended {
        code,
        job,
        tail: lines.last(SHOWN_ON_FAILURE),
    })
}

/// What a command that started a job does with it: print its id and leave
/// with `--detach`, or follow it to its end. The exit status is the job's:
/// 1 when it failed, or its own when `own` says so, as for a command on a host.
pub fn acted(
    client: &Client,
    acted: rookery_proto::hosts::Acted,
    title: &str,
    detach: bool,
    own: bool,
) -> Result<Value> {
    let Some(job) = acted.job else {
        if let Some(result) = &acted.result {
            return Ok(result.clone());
        }
        if let Some(session) = &acted.session {
            return Ok(serde_json::to_value(session)?);
        }
        return Ok(Value::Null);
    };
    if detach {
        let info = serde_json::to_value(&acted.info)?;
        out::data(&info, || println!("{job}"));
        return Ok(Value::Null);
    }
    let ended = self::job(client, &job, title, own)?;
    if out::get().json {
        out::json(&ended.job);
    }
    match (ended.code, own) {
        (0, _) => Ok(ended.job),
        (code, true) => Err(Fail::exit(code)),
        (_, false) => Err(Fail::exit(1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tail_is_the_last_lines_without_their_colours() {
        let mut lines = Lines::default();
        lines.take(b"TASK [packages]\n\x1b[32mok: [studio]\x1b[0m\n");
        lines.take(b"progress 10%\rprogress 60%");
        assert_eq!(lines.last(2), ["ok: [studio]", "progress 60%"]);
        lines.take(b"\n\n");
        assert_eq!(
            lines.last(5),
            ["TASK [packages]", "ok: [studio]", "progress 60%"]
        );
    }
}

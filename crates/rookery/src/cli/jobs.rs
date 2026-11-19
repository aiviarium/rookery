//! `rookery job …` and `rookery rollout`: the work the server does, followed
//! again or started over all hosts.

use super::args::{self, Job};
use super::client::{Client, Subject};
use super::fail::{Fail, Result};
use super::follow;
use super::out::{self, Style, Table, styled};
use rookery_api::Act;
use rookery_api::portal::Piece;
use serde_json::{Value, json};
use std::io::Write;

/// A job's arguments as words a shell reads back; `None` when it has none.
fn job_arguments(job: &Value) -> Option<String> {
    let arguments: Vec<String> = job["arguments"]
        .as_array()?
        .iter()
        .filter_map(|word| word.as_str().map(str::to_owned))
        .collect();
    (!arguments.is_empty()).then(|| shell_words(&arguments))
}

/// Words as a shell reads them back: a word with a space or a character the
/// shell gives a meaning is quoted.
fn shell_words(words: &[String]) -> String {
    let quote = |word: &String| {
        let plain = !word.is_empty()
            && word
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || "-_./=:,+@%^".contains(ch));
        match plain {
            true => word.clone(),
            false => format!("'{}'", word.replace('\'', r"'\''")),
        }
    };
    words.iter().map(quote).collect::<Vec<_>>().join(" ")
}

/// A line per host of a roll-out's report.
fn summary(report: &rookery_api::RolloutReport) -> Vec<String> {
    use rookery_api::RolloutState::*;
    let mut lines = vec![];
    for host in &report.hosts {
        let state = match host.state {
            Done if host.rebooted => "up to date, rebooted",
            Done => "up to date",
            Current => "was current",
            Skipped => "not started",
            Failed => "failed",
        };
        let mut line = format!("{}: {state}", host.name);
        if !host.error.is_empty() {
            line += &format!(": {}", host.error);
        }
        for warning in &host.warnings {
            line += &format!("; {warning}");
        }
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push("No hosts".into());
    }
    lines
}

fn state_of(job: &Value) -> String {
    if job["ended"].is_null() {
        return styled("running", Style::Accent);
    }
    match job["exit_code"].as_i64() {
        Some(0) => styled("done", Style::Good),
        Some(code) => styled(&format!("failed ({code})"), Style::Bad),
        None => styled("failed", Style::Bad),
    }
}

pub fn command(client: &Client, command: Job) -> Result<()> {
    match command {
        Job::List {
            limit,
            host,
            subject,
            running,
            before,
        } => {
            let mut args = json!({"limit": limit});
            for (key, value) in [("host", host), ("subject", subject), ("before", before)] {
                if let Some(value) = value {
                    args[key] = json!(value);
                }
            }
            if running {
                args["running"] = json!(true);
            }
            let found = client.call("jobs.list", args, None)?;
            out::data(&found, || {
                let mut table = Table::new(&["id", "action", "subject", "state", "started"]);
                for job in found.as_array().into_iter().flatten() {
                    let text = |key: &str| job[key].as_str().unwrap_or("").to_owned();
                    let subject = match text("host").as_str() {
                        "" => text("subject"),
                        host => host.to_owned(),
                    };
                    table.row(vec![
                        text("id"),
                        text("action"),
                        subject,
                        state_of(job),
                        text("started"),
                    ]);
                }
                table.print("No jobs yet");
            });
            Ok(())
        }
        Job::Show { id } => {
            let job = client.call("jobs.get", json!({"id": id}), None)?;
            out::data(&job, || {
                let text = |key: &str| job[key].as_str().unwrap_or("").to_owned();
                out::fields(&[
                    ("Id", text("id")),
                    ("Action", text("action")),
                    ("Subject", text("subject")),
                    ("Host", text("host")),
                    (
                        match text("action").as_str() {
                            "exec" => "Command",
                            _ => "Arguments",
                        },
                        job_arguments(&job).unwrap_or_default(),
                    ),
                    ("State", state_of(&job)),
                    ("Started", text("started")),
                    ("Ended", text("ended")),
                ]);
                if let Some(report) = job["report"].as_object() {
                    let report: rookery_api::RolloutReport =
                        serde_json::from_value(Value::Object(report.clone())).unwrap_or_default();
                    for line in summary(&report) {
                        println!("{line}");
                    }
                }
            });
            Ok(())
        }
        Job::Logs { id, follow } => logs(client, &id, follow),
        Job::Stop { id } => {
            let job = client.call("jobs.stop", json!({"id": id}), None)?;
            out::data(&job, || {
                out::done(format!(
                    "Asked {id} to stop. rookery job wait {id} waits for its end"
                ))
            });
            Ok(())
        }
        Job::Wait { id } => {
            let job = client.call("jobs.get", json!({"id": id}), None)?;
            let title = format!(
                "{} {}",
                job["action"].as_str().unwrap_or("job"),
                job["host"].as_str().unwrap_or("")
            );
            let ended = follow::job(client, &id, title.trim(), false)?;
            if out::get().json {
                out::json(&ended.job);
            }
            match ended.code {
                0 => Ok(()),
                _ => Err(Fail::exit(1)),
            }
        }
    }
}

/// What a job wrote, with its colours where they are shown, and with
/// `follow` what it writes until it ends.
fn logs(client: &Client, id: &str, follow: bool) -> Result<()> {
    let colour = out::get().colour.0;
    let write = |bytes: &[u8]| -> std::io::Result<()> {
        let mut stdout = std::io::stdout().lock();
        match colour {
            true => stdout.write_all(bytes)?,
            false => {
                stdout.write_all(out::strip_ansi(&String::from_utf8_lossy(bytes)).as_bytes())?
            }
        }
        stdout.flush()
    };
    if !follow {
        let found = client.call("jobs.output", json!({"id": id}), None)?;
        let bytes = match &found["data_b64"] {
            Value::String(data) => rookery_api::unb64(data).unwrap_or_default(),
            _ => match &found["lines"] {
                Value::Array(lines) => lines
                    .iter()
                    .map(|line| format!("{}\n", line.as_str().unwrap_or("")))
                    .collect::<String>()
                    .into_bytes(),
                _ => found["base64"]
                    .as_str()
                    .and_then(rookery_api::unb64)
                    .unwrap_or_default(),
            },
        };
        write(&bytes)?;
        return Ok(());
    }
    let mut stream = client.stream("jobs.follow", None, json!({"id": id}))?;
    let code = client.wait(async {
        let mut code = None;
        loop {
            tokio::select! {
                piece = stream.recv() => match piece {
                    Some(Ok(Piece::Binary(bytes))) => write(&bytes)?,
                    Some(Ok(Piece::Text(said))) => {
                        if let Some(exited) = said.get("exited") {
                            code = Some(exited["exit_code"].as_i64().unwrap_or(1) as i32);
                        }
                    }
                    Some(Err(error)) => return Err(Fail::from(error)),
                    None => return Ok(code),
                },
                _ = tokio::signal::ctrl_c() => return Ok(Some(0)),
            }
        }
    })?;
    match code.unwrap_or(0) {
        0 => Ok(()),
        _ => Err(Fail::exit(1)),
    }
}

/// `rookery rollout`: the installation's roll-out, as a job of the server.
pub fn rollout(client: &Client, rollout: &args::Rollout) -> Result<()> {
    let options = rookery_api::Rollout {
        hosts: rollout.only.clone(),
        except: rollout.except.clone(),
        all: rollout.all,
        update: rollout.update,
        reboot: rollout.reboot,
        force: rollout.force,
    };
    let act = Act {
        arguments: options.arguments(),
        ..Act::default()
    };
    let acted = client.act(&Subject::Installation, "rollout", &act)?;
    let (Some(job), false) = (&acted.job, rollout.detach.detach) else {
        follow::acted(client, acted, "Rolling out", rollout.detach.detach, false)?;
        return Ok(());
    };
    // The line of each host is said whether the roll-out ended well or not.
    let ended = follow::job(client, &job.id, "Rolling out", false)?;
    if out::get().json {
        out::json(&ended.job);
    } else if ended.job["report"].is_object() {
        let report: rookery_api::RolloutReport =
            serde_json::from_value(ended.job["report"].clone()).unwrap_or_default();
        for line in summary(&report) {
            println!("{line}");
        }
    }
    match ended.code {
        0 => Ok(()),
        _ => Err(Fail::exit(1)),
    }
}

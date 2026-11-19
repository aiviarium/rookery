//! `rookery job …` and `rookery rollout`: the work the server does, followed
//! again or started over all hosts.

use super::args::{self, Job};
use super::client::{Client, Subject};
use super::fail::{Fail, Result};
use super::follow;
use super::out::{self, Style, Table, styled};
use rookery_api::Act;
use serde_json::{Value, json};
use std::io::Write;

/// What a job does, its hosts, its exit code and when it started, as its record says them.
pub fn action_of(job: &Value) -> String {
    job["params"]["action"]
        .as_str()
        .unwrap_or_else(|| job["kind"].as_str().unwrap_or(""))
        .to_owned()
}

pub fn host_of(job: &Value) -> String {
    job["params"]["host"].as_str().unwrap_or("").to_owned()
}

/// The exit code a job ended with: in its result, or in its error's details.
pub fn exit_of(job: &Value) -> Option<i32> {
    job["result"]["exit"]
        .as_i64()
        .or_else(|| job["error"]["details"]["exit"].as_i64())
        .map(|code| code as i32)
}

/// A job's arguments as words a shell reads back; `None` when it has none.
fn job_arguments(job: &Value) -> Option<String> {
    let arguments: Vec<String> = job["params"]["arguments"]
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
    match job["state"].as_str().unwrap_or("") {
        "queued" => styled("queued", Style::Accent),
        "running" => styled("running", Style::Accent),
        "succeeded" => styled("done", Style::Good),
        "cancelled" => styled("stopped", Style::Bad),
        _ => match exit_of(job) {
            Some(code) => styled(&format!("failed ({code})"), Style::Bad),
            None => styled("failed", Style::Bad),
        },
    }
}

/// The resource a subject given on the command line is: a host by its name, or `KIND:NAME`.
fn subject_uri(given: &str) -> String {
    match given.split_once(':') {
        Some(("host", name)) => format!("env://{name}"),
        Some(("provisioner", name)) => format!("provisioner://{name}"),
        Some(("resource", id)) => format!("resource://{id}"),
        _ if given == "installation" => "rookery://installation".into(),
        _ if given.contains("://") => given.to_owned(),
        _ => format!("env://{given}"),
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
            if let Some(subject) = subject.or(host) {
                args["subject"] = json!(subject_uri(&subject));
            }
            if let Some(before) = before {
                args["before"] = json!(before);
            }
            if running {
                args["state"] = json!("running");
            }
            let found = client.call("jobs.list", args)?;
            let jobs = found["jobs"].clone();
            out::data(&jobs, || {
                let mut table = Table::new(&["id", "action", "subject", "state", "started"]);
                for job in jobs.as_array().into_iter().flatten() {
                    let text = |key: &str| job[key].as_str().unwrap_or("").to_owned();
                    let subject = match host_of(job).as_str() {
                        "" => text("subject"),
                        host => host.to_owned(),
                    };
                    table.row(vec![
                        text("id"),
                        action_of(job),
                        subject,
                        state_of(job),
                        text("createdAt"),
                    ]);
                }
                table.print("No jobs yet");
            });
            Ok(())
        }
        Job::Show { id } => {
            let job = client.call("jobs.get", json!({"id": id}))?;
            out::data(&job, || {
                let text = |key: &str| job[key].as_str().unwrap_or("").to_owned();
                out::fields(&[
                    ("Id", text("id")),
                    ("Action", action_of(&job)),
                    ("Subject", text("subject")),
                    ("Host", host_of(&job)),
                    (
                        match action_of(&job).as_str() {
                            "exec" => "Command",
                            _ => "Arguments",
                        },
                        job_arguments(&job).unwrap_or_default(),
                    ),
                    ("State", state_of(&job)),
                    ("Started", text("startedAt")),
                    ("Ended", text("finishedAt")),
                ]);
                if let Some(report) = job["result"]["report"]
                    .as_object()
                    .or_else(|| job["error"]["details"]["report"].as_object())
                {
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
            let job = client.call("jobs.cancel", json!({"id": id}))?;
            out::data(&job, || {
                out::done(format!(
                    "Asked {id} to stop. rookery job wait {id} waits for its end"
                ))
            });
            Ok(())
        }
        Job::Wait { id } => {
            let job = client.call("jobs.get", json!({"id": id}))?;
            let title = format!("{} {}", action_of(&job), host_of(&job));
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
        let mut after = 0;
        loop {
            let found = client.call("jobs.output", json!({"id": id, "after": after}))?;
            let items = found["items"].as_array().cloned().unwrap_or_default();
            for item in &items {
                after = item["seq"].as_i64().unwrap_or(after);
                write(format!("{}\n", item["text"].as_str().unwrap_or("")).as_bytes())?;
            }
            if items.len() < 1000 {
                return Ok(());
            }
        }
    }
    let mut stream = client.stream("jobs.follow", json!({"id": id, "after": 0}))?;
    let job = client.wait(async {
        let mut buffer = vec![];
        loop {
            tokio::select! {
                line = stream.line(&mut buffer) => match line.map_err(Fail::from)? {
                    Some(line) if line.get("seq").is_some() => {
                        write(format!("{}\n", line["text"].as_str().unwrap_or("")).as_bytes())?;
                    }
                    Some(end) => return Ok::<_, Fail>(Some(end)),
                    None => return Ok(None),
                },
                _ = tokio::signal::ctrl_c() => return Ok(None),
            }
        }
    })?;
    match job {
        Some(end) if end["state"] != "succeeded" => Err(Fail::exit(1)),
        _ => Ok(()),
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
    let ended = follow::job(client, job, "Rolling out", false)?;
    let report = match ended.job["result"]["report"].is_object() {
        true => ended.job["result"]["report"].clone(),
        false => ended.job["error"]["details"]["report"].clone(),
    };
    if out::get().json {
        out::json(&ended.job);
    } else if report.is_object() {
        let report: rookery_api::RolloutReport = serde_json::from_value(report).unwrap_or_default();
        for line in summary(&report) {
            println!("{line}");
        }
    }
    match ended.code {
        0 => Ok(()),
        _ => Err(Fail::exit(1)),
    }
}

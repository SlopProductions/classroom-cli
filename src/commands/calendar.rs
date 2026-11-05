use super::tasks::{ListCache, ListOpts, fetch_tasks, load_cache, save_cache};
use crate::Ctx;
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Local, Utc};
use std::path::PathBuf;

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\n', "\\n")
}

fn fold(line: &str) -> String {
    let mut out = String::new();
    let mut width = 0;
    for ch in line.chars() {
        let len = ch.len_utf8();
        if width + len > 74 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(ch);
        width += len;
    }
    out.push_str("\r\n");
    out
}

fn stamp(d: DateTime<Utc>) -> String {
    d.format("%Y%m%dT%H%M%SZ").to_string()
}

pub fn calendar(ctx: &Ctx, course: Option<&str>, output: Option<PathBuf>) -> Result<()> {
    let web = ctx.web()?;
    let rows = fetch_tasks(
        &web,
        &ListOpts {
            course,
            done: false,
            old: false,
        },
    )?;
    let now = stamp(Utc::now());
    let mut ics = String::new();
    for line in [
        "BEGIN:VCALENDAR",
        "VERSION:2.0",
        "PRODID:-//class//Google Classroom CLI//EN",
        "CALSCALE:GREGORIAN",
        "X-WR-CALNAME:Classroom",
    ] {
        ics.push_str(&fold(line));
    }
    let mut events = 0;
    for r in &rows {
        let Some(due) = r.task.due else { continue };
        let due = due.with_timezone(&Utc);
        let link = r.task.link();
        events += 1;
        for line in [
            "BEGIN:VEVENT".to_string(),
            format!("UID:{}-{}@classroom-cli", r.course.id, r.task.id),
            format!("DTSTAMP:{now}"),
            format!("DTSTART:{}", stamp(due - Duration::minutes(30))),
            format!("DTEND:{}", stamp(due)),
            format!(
                "SUMMARY:{}",
                escape(&format!("{}: {}", r.course.name, r.task.title))
            ),
            format!("DESCRIPTION:{}", escape(&link)),
            format!("URL:{link}"),
            "BEGIN:VALARM".to_string(),
            "ACTION:DISPLAY".to_string(),
            format!("DESCRIPTION:{}", escape(&r.task.title)),
            "TRIGGER:-P1D".to_string(),
            "END:VALARM".to_string(),
            "END:VEVENT".to_string(),
        ] {
            ics.push_str(&fold(&line));
        }
    }
    ics.push_str(&fold("END:VCALENDAR"));

    match output {
        Some(path) => {
            std::fs::write(&path, &ics)
                .with_context(|| format!("Could not write {}", path.display()))?;
            eprintln!("Wrote {events} assignments to {}", path.display());
        }
        None => print!("{ics}"),
    }
    Ok(())
}

fn refresh_cache(ctx: &Ctx) -> Result<ListCache> {
    let web = ctx.web()?;
    let rows = fetch_tasks(
        &web,
        &ListOpts {
            course: None,
            done: false,
            old: false,
        },
    )?;
    save_cache(&web.email, &rows);
    load_cache().context("Could not write the assignment cache")
}

fn refresh_in_background() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe)
            .args(["count", "--refresh"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

pub fn count(ctx: &Ctx, refresh: bool, days: i64) -> Result<()> {
    let cached = load_cache().filter(|c| c.fetched_at.is_some());
    let cache = match cached {
        Some(c) if !refresh => {
            let age = c
                .fetched_at
                .as_deref()
                .and_then(|f| DateTime::parse_from_rfc3339(f).ok())
                .map(|f| Local::now() - f.with_timezone(&Local));
            if age.is_none_or(|a| a > Duration::hours(1)) {
                refresh_in_background();
            }
            c
        }
        _ => refresh_cache(ctx)?,
    };
    let limit = Local::now() + Duration::days(days);
    let n = cache
        .tasks
        .iter()
        .filter(|t| !t.done)
        .filter(|t| {
            t.due
                .as_deref()
                .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
                .is_some_and(|d| d.with_timezone(&Local) <= limit)
        })
        .count();
    println!("{n}");
    Ok(())
}

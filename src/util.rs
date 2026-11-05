use crate::classroom::{Course, Task};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use chrono::{DateTime, Datelike, Local, Weekday};
use comfy_table::Color;
use std::path::PathBuf;

pub fn slug(s: &str) -> String {
    let ascii = deunicode::deunicode(s).to_lowercase();
    let mut out = String::new();
    for ch in ascii.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(50);
    let out = out.trim_matches('-').to_string();
    if out.is_empty() { "task".into() } else { out }
}

pub fn folder_for(template: &str, course: &Course, cw: &Task) -> PathBuf {
    let rendered = template
        .replace("{course}", &slug(&course.name))
        .replace("{title}", &slug(&cw.title))
        .replace("{section}", &slug(course.section.as_deref().unwrap_or("")));
    let mut p = PathBuf::new();
    for part in rendered.split('/') {
        if !part.is_empty() && part != "." && part != ".." {
            p.push(part);
        }
    }
    if p.as_os_str().is_empty() {
        p.push(slug(&cw.title));
    }
    p
}

fn weekday(w: Weekday) -> &'static str {
    match w {
        Weekday::Mon => "Mon",
        Weekday::Tue => "Tue",
        Weekday::Wed => "Wed",
        Weekday::Thu => "Thu",
        Weekday::Fri => "Fri",
        Weekday::Sat => "Sat",
        Weekday::Sun => "Sun",
    }
}

pub fn fmt_due(due: Option<DateTime<Local>>) -> (String, Option<Color>) {
    let Some(due) = due else {
        return ("no due date".into(), Some(Color::DarkGrey));
    };
    let now = Local::now();
    let days = (due.date_naive() - now.date_naive()).num_days();
    let time = due.format("%H:%M");
    if due < now {
        return (
            format!("overdue ({})", due.format("%b %d %H:%M")),
            Some(Color::Red),
        );
    }
    match days {
        0 => (format!("today {time}"), Some(Color::Yellow)),
        1 => (format!("tomorrow {time}"), Some(Color::Yellow)),
        2..=6 => (
            format!("{} {}", weekday(due.weekday()), due.format("%b %d %H:%M")),
            None,
        ),
        _ => (due.format("%Y-%m-%d %H:%M").to_string(), None),
    }
}

pub fn fmt_due_long(due: Option<DateTime<Local>>) -> String {
    due.map(|d| format!("{} ({})", d.format("%Y-%m-%d %H:%M"), weekday(d.weekday())))
        .unwrap_or_else(|| "none".into())
}

pub fn with_authuser(link: &str, email: &str) -> String {
    match url::Url::parse(link) {
        Ok(mut u) => {
            u.query_pairs_mut().append_pair("authuser", email);
            u.to_string()
        }
        Err(_) => link.to_string(),
    }
}

pub fn parse_classroom_url(s: &str) -> Option<(String, String)> {
    let u = url::Url::parse(s).ok()?;
    let segs: Vec<&str> = u.path_segments()?.collect();
    let ci = segs.iter().position(|s| *s == "c")?;
    let course = segs.get(ci + 1)?;
    let wi = segs[ci + 2..]
        .iter()
        .position(|s| matches!(*s, "a" | "sa" | "mc" | "q"))?
        + ci
        + 2;
    let work = segs.get(wi + 1)?;
    Some((decode_id(course), decode_id(work)))
}

fn decode_id(s: &str) -> String {
    if s.chars().all(|c| c.is_ascii_digit()) {
        return s.to_string();
    }
    for engine in [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD] {
        if let Ok(bytes) = engine.decode(s)
            && let Ok(text) = String::from_utf8(bytes)
            && !text.is_empty()
            && text.chars().all(|c| c.is_ascii_digit())
        {
            return text;
        }
    }
    s.to_string()
}

pub fn confirm(prompt: &str, default: bool) -> anyhow::Result<bool> {
    Ok(dialoguer::Confirm::new()
        .with_prompt(prompt)
        .default(default)
        .interact()?)
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slug("Matematyka 3B"), "matematyka-3b");
        assert_eq!(slug("  Zadanie: Źdźbło trawy!! "), "zadanie-zdzblo-trawy");
        assert_eq!(slug("???"), "task");
    }

    #[test]
    fn classroom_url() {
        assert_eq!(
            parse_classroom_url("https://classroom.google.com/u/1/c/MTIz/a/NDU2/details"),
            Some(("123".into(), "456".into()))
        );
        assert_eq!(
            parse_classroom_url("https://classroom.google.com/c/MTIz/sa/NDU2/details"),
            Some(("123".into(), "456".into()))
        );
        assert_eq!(
            parse_classroom_url("https://classroom.google.com/c/MTIz"),
            None
        );
    }
}

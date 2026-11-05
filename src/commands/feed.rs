use super::tasks::{filter_courses, per_course};
use crate::Ctx;
use crate::classroom::{Course, Post, PostKind, task_link};
use crate::config;
use crate::progress::{Spinner, spin};
use anyhow::Result;
use chrono::{DateTime, Duration, Local};
use console::style;
use std::collections::HashMap;

fn seen_path() -> std::path::PathBuf {
    config::cache_dir().join("feed_seen.json")
}

fn load_seen() -> HashMap<String, String> {
    std::fs::read(seen_path())
        .ok()
        .and_then(|d| serde_json::from_slice(&d).ok())
        .unwrap_or_default()
}

fn save_seen(seen: &HashMap<String, String>) {
    let _ = std::fs::create_dir_all(config::cache_dir());
    let _ = std::fs::write(seen_path(), serde_json::to_vec(seen).unwrap_or_default());
}

fn snippet(text: &str, max: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > max {
        format!("{}…", line.chars().take(max).collect::<String>())
    } else {
        line
    }
}

fn kind_label(kind: PostKind) -> String {
    let label = format!("{:<12}", kind.label());
    match kind {
        PostKind::Assignment => style(label).cyan().to_string(),
        PostKind::Announcement => style(label).yellow().to_string(),
        PostKind::Question => style(label).magenta().to_string(),
        PostKind::Material => style(label).blue().to_string(),
    }
}

pub fn run(ctx: &Ctx, course: Option<&str>, days: i64, unread: bool) -> Result<()> {
    let web = ctx.web()?;
    let courses = filter_courses(spin("Fetching courses", || web.courses())?, course);
    let spinner = Spinner::new("Fetching course streams");
    let results = per_course(&courses, |c| web.stream(&c.id));
    drop(spinner);

    let mut seen = load_seen();
    let last_seen = seen
        .get(&web.email)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Local));
    let cutoff = Local::now() - Duration::days(days);

    let mut posts: Vec<(Course, Post)> = vec![];
    for (c, res) in results {
        match res {
            Ok(p) => posts.extend(p.into_iter().map(|p| (c.clone(), p))),
            Err(e) => eprintln!(
                "{} skipped course '{}': {e:#}",
                style("Warning:").yellow(),
                c.name
            ),
        }
    }
    let is_new = |p: &Post| match (p.created, last_seen) {
        (Some(c), Some(l)) => c > l,
        (_, None) => true,
        _ => false,
    };
    posts.retain(|(_, p)| p.created.is_some_and(|c| c > cutoff) && (!unread || is_new(p)));
    posts.sort_by_key(|p| std::cmp::Reverse(p.1.created));

    if posts.is_empty() {
        println!("Nothing new in the last {days} days");
    }
    for (c, p) in &posts {
        let date = p
            .created
            .map(|d| d.format("%b %d %H:%M").to_string())
            .unwrap_or_default();
        let marker = if is_new(p) {
            style("●").green().to_string()
        } else {
            " ".into()
        };
        let headline = if p.title.is_empty() {
            p.text
                .as_deref()
                .map(|t| snippet(t, 90))
                .unwrap_or_default()
        } else {
            p.title.clone()
        };
        println!(
            "{marker} {}  {}  {}  {}",
            style(date).dim(),
            kind_label(p.kind),
            style(headline).bold(),
            style(&c.name).dim()
        );
        if !p.title.is_empty()
            && let Some(t) = &p.text
        {
            println!(
                "                               {}",
                style(snippet(t, 90)).dim()
            );
        }
        if p.kind == PostKind::Assignment {
            println!(
                "                               {}",
                style(task_link(&p.course_id, &p.id)).dim()
            );
        }
    }

    seen.insert(web.email.clone(), Local::now().to_rfc3339());
    save_seen(&seen);
    Ok(())
}

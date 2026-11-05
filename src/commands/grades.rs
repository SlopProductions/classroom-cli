use super::tasks::{filter_courses, per_course};
use crate::Ctx;
use crate::classroom::{Course, Submission, Task};
use crate::progress::{Spinner, spin};
use anyhow::Result;
use comfy_table::{Cell, CellAlignment, Color, ContentArrangement, Table, presets};
use console::style;
use std::collections::{BTreeMap, HashMap};

struct Graded {
    course: Course,
    task: Task,
    grade: f64,
}

fn percent(grade: f64, max: f64) -> Option<f64> {
    (max > 0.0).then(|| grade / max * 100.0)
}

fn percent_cell(p: Option<f64>) -> Cell {
    let Some(p) = p else {
        return Cell::new("-").fg(Color::DarkGrey);
    };
    let color = if p >= 75.0 {
        Color::Green
    } else if p >= 50.0 {
        Color::Yellow
    } else {
        Color::Red
    };
    Cell::new(format!("{p:.0}%"))
        .fg(color)
        .set_alignment(CellAlignment::Right)
}

fn fmt_points(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

pub fn run(ctx: &Ctx, course: Option<&str>, summary: bool) -> Result<()> {
    let web = ctx.web()?;
    let courses = filter_courses(spin("Fetching courses", || web.courses())?, course);
    let spinner = Spinner::new("Fetching grades");
    let results = per_course(&courses, |c| {
        let mut tasks = web.tasks(&c.id, true)?;
        tasks.extend(web.tasks(&c.id, false)?);
        let ids: Vec<String> = tasks.iter().map(|t| t.id.clone()).collect();
        let subs: HashMap<String, Submission> = web.submissions(&c.id, &ids)?.into_iter().collect();
        Ok(tasks
            .into_iter()
            .filter_map(|t| {
                let grade = subs.get(&t.id)?.grade?;
                Some(Graded {
                    course: c.clone(),
                    task: t,
                    grade,
                })
            })
            .collect::<Vec<_>>())
    });
    drop(spinner);

    let mut graded = vec![];
    for (c, res) in results {
        match res {
            Ok(g) => graded.extend(g),
            Err(e) => eprintln!(
                "{} skipped course '{}': {e:#}",
                style("Warning:").yellow(),
                c.name
            ),
        }
    }
    if graded.is_empty() {
        println!("No grades yet");
        return Ok(());
    }
    graded.sort_by(|a, b| {
        a.course
            .name
            .cmp(&b.course.name)
            .then(b.task.due.cmp(&a.task.due))
    });

    let mut t = Table::new();
    t.load_preset(presets::NOTHING)
        .set_content_arrangement(ContentArrangement::Dynamic);
    let mut totals: BTreeMap<String, (usize, f64, f64)> = BTreeMap::new();
    for g in &graded {
        let entry = totals.entry(g.course.name.clone()).or_default();
        entry.0 += 1;
        if let Some(max) = g.task.max_points {
            entry.1 += g.grade;
            entry.2 += max;
        }
    }

    if summary {
        t.set_header(
            ["Course", "Graded", "Points", "Score"].map(|h| Cell::new(h).fg(Color::DarkGrey)),
        );
        for (name, (count, got, max)) in &totals {
            t.add_row(vec![
                Cell::new(name),
                Cell::new(count).set_alignment(CellAlignment::Right),
                Cell::new(format!("{}/{}", fmt_points(*got), fmt_points(*max)))
                    .set_alignment(CellAlignment::Right),
                percent_cell(percent(*got, *max)),
            ]);
        }
    } else {
        t.set_header(
            ["Course", "Assignment", "Grade", "Score"].map(|h| Cell::new(h).fg(Color::DarkGrey)),
        );
        for g in &graded {
            let grade = match g.task.max_points {
                Some(max) => format!("{}/{}", fmt_points(g.grade), fmt_points(max)),
                None => fmt_points(g.grade),
            };
            t.add_row(vec![
                Cell::new(&g.course.name),
                Cell::new(&g.task.title),
                Cell::new(grade).set_alignment(CellAlignment::Right),
                percent_cell(g.task.max_points.and_then(|m| percent(g.grade, m))),
            ]);
        }
    }
    println!("{t}");

    let (got, max) = totals
        .values()
        .fold((0.0, 0.0), |acc, v| (acc.0 + v.1, acc.1 + v.2));
    if let Some(p) = percent(got, max) {
        println!(
            "{}",
            style(format!(
                "\n{} graded assignments · overall {p:.0}%",
                graded.len()
            ))
            .dim()
        );
    }
    Ok(())
}

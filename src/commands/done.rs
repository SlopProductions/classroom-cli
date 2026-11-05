use super::tasks::{ListOpts, TaskRow, fetch_tasks, resolve_ref};
use crate::Ctx;
use crate::classroom::State;
use crate::progress::{Spinner, spin};
use crate::util::confirm;
use crate::web::Web;
use anyhow::{Result, bail};
use chrono::{Duration, Local};
use console::style;

pub struct DoneArgs {
    pub task: Option<String>,
    pub older_than: u32,
    pub course: Option<String>,
    pub dry_run: bool,
    pub yes: bool,
}

fn mark(web: &Web, course_id: &str, task_id: &str) -> Result<()> {
    let after = web.turn_in(course_id, task_id)?;
    if after.state != State::TurnedIn {
        bail!(
            "Classroom did not confirm (status: {})",
            after.state.label()
        );
    }
    Ok(())
}

fn is_old(row: &TaskRow, days: u32) -> bool {
    let cutoff = Local::now() - Duration::days(days.into());
    match row.task.due {
        Some(due) => due < cutoff,
        None => row.task.created.is_some_and(|c| c < cutoff),
    }
}

pub fn run(ctx: &Ctx, args: DoneArgs) -> Result<()> {
    let web = ctx.web()?;
    if let Some(r) = &args.task {
        let (course_id, task_id) = resolve_ref(&web.email, r)?;
        let task = spin("Fetching assignment", || web.task(&course_id, &task_id))?;
        if task.done {
            println!("'{}' is already turned in", task.title);
            return Ok(());
        }
        if args.dry_run {
            println!("Would mark as done: {}", task.title);
            return Ok(());
        }
        if !args.yes
            && !confirm(
                &format!("Mark '{}' as done without attaching anything?", task.title),
                false,
            )?
        {
            return Ok(());
        }
        spin("Marking as done", || mark(&web, &course_id, &task_id))?;
        println!(
            "{} Marked as done: {}",
            style("✓").green().bold(),
            task.title
        );
        return Ok(());
    }

    let rows: Vec<TaskRow> = fetch_tasks(
        &web,
        &ListOpts {
            course: args.course.as_deref(),
            done: false,
            old: true,
        },
    )?
    .into_iter()
    .filter(|r| is_old(r, args.older_than))
    .collect();

    if rows.is_empty() {
        println!("No pending assignments older than {} days", args.older_than);
        return Ok(());
    }
    for r in &rows {
        let due = r
            .task
            .due
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "no due date".into());
        println!(
            "  {} {}  {}",
            style("•").dim(),
            r.task.title,
            style(format!("{} · {}", r.course.name, due)).dim()
        );
    }
    println!(
        "\n{} assignments older than {} days will be marked as done without attachments",
        rows.len(),
        args.older_than
    );
    if args.dry_run {
        return Ok(());
    }
    if !args.yes && !confirm("Mark all of them as done?", false)? {
        return Ok(());
    }

    let spinner = Spinner::new("Marking as done");
    let mut failed = 0;
    for (i, r) in rows.iter().enumerate() {
        spinner.set(format!(
            "Marking {}/{}: {}",
            i + 1,
            rows.len(),
            r.task.title
        ));
        if let Err(e) = mark(&web, &r.course.id, &r.task.id) {
            failed += 1;
            spinner.println(format!("  {} {}: {e:#}", style("✗").red(), r.task.title));
        }
    }
    drop(spinner);
    let ok = rows.len() - failed;
    println!(
        "{} Marked {ok} assignments as done",
        style("✓").green().bold()
    );
    if failed > 0 {
        bail!("{failed} assignments could not be marked");
    }
    Ok(())
}

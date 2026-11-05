use crate::Ctx;
use crate::classroom::{Course, Task, task_link};
use crate::config;
use crate::progress::{Spinner, spin};
use crate::util::{fmt_due, parse_classroom_url, with_authuser};
use crate::web::Web;
use crate::workspace;
use anyhow::{Context, Result, bail};
use chrono::{Duration, Local};
use comfy_table::{Cell, Color, ContentArrangement, Table, presets};
use console::style;
use serde::{Deserialize, Serialize};

const OLD_AFTER_DAYS: i64 = 30;

pub struct TaskRow {
    pub course: Course,
    pub task: Task,
}

pub struct ListOpts<'a> {
    pub course: Option<&'a str>,
    pub done: bool,
    pub old: bool,
}

pub fn fetch_tasks(web: &Web, opts: &ListOpts) -> Result<Vec<TaskRow>> {
    let spinner = Spinner::new("Fetching assignments");
    let filter = opts.course.map(|f| deunicode::deunicode(f).to_lowercase());
    let courses: Vec<Course> = web
        .courses()?
        .into_iter()
        .filter(|c| {
            filter
                .as_ref()
                .is_none_or(|f| deunicode::deunicode(&c.name).to_lowercase().contains(f))
        })
        .collect();

    let groups: Vec<bool> = if opts.done {
        vec![false, true]
    } else {
        vec![false]
    };
    let results: Vec<(String, Result<Vec<TaskRow>>)> = std::thread::scope(|s| {
        let handles: Vec<_> = courses
            .iter()
            .flat_map(|c| groups.iter().map(move |done| (c, *done)))
            .map(|(c, done)| {
                let handle = s.spawn(move || -> Result<Vec<TaskRow>> {
                    Ok(web
                        .tasks(&c.id, done)?
                        .into_iter()
                        .map(|task| TaskRow {
                            course: c.clone(),
                            task,
                        })
                        .collect())
                });
                (c.name.clone(), handle)
            })
            .collect();
        handles
            .into_iter()
            .map(|(name, h)| (name, h.join().expect("fetch thread panicked")))
            .collect()
    });

    drop(spinner);
    let mut rows = vec![];
    for (name, res) in results {
        match res {
            Ok(r) => rows.extend(r),
            Err(e) => eprintln!(
                "{} skipped course '{name}': {e:#}",
                style("Warning:").yellow()
            ),
        }
    }
    if !opts.old {
        let cutoff = Local::now() - Duration::days(OLD_AFTER_DAYS);
        rows.retain(|r| {
            r.task.done
                || match r.task.due {
                    Some(d) => d > cutoff,
                    None => r.task.created.is_none_or(|c| c > cutoff),
                }
        });
    }
    rows.sort_by_key(|r| (r.task.due.is_none(), r.task.due));
    Ok(rows)
}

#[derive(Serialize, Deserialize)]
struct ListCache {
    account: String,
    tasks: Vec<CachedTask>,
}

#[derive(Serialize, Deserialize)]
struct CachedTask {
    course_id: String,
    course_work_id: String,
}

fn cache_path() -> std::path::PathBuf {
    config::cache_dir().join("last_list.json")
}

fn save_cache(account: &str, rows: &[TaskRow]) {
    let cache = ListCache {
        account: account.into(),
        tasks: rows
            .iter()
            .map(|r| CachedTask {
                course_id: r.course.id.clone(),
                course_work_id: r.task.id.clone(),
            })
            .collect(),
    };
    let _ = std::fs::create_dir_all(config::cache_dir());
    let _ = std::fs::write(cache_path(), serde_json::to_vec(&cache).unwrap_or_default());
}

pub fn resolve_ref(email: &str, r: &str) -> Result<(String, String)> {
    if let Ok(n) = r.parse::<usize>() {
        let data = std::fs::read(cache_path())
            .context("Run `class list` first to pick assignments by number")?;
        let cache: ListCache = serde_json::from_slice(&data)?;
        if cache.account != email {
            bail!(
                "The last list was for account {}, but you are using {email}. Run `class list` again.",
                cache.account
            );
        }
        let t = n
            .checked_sub(1)
            .and_then(|i| cache.tasks.get(i))
            .with_context(|| format!("No assignment #{n} on the last list"))?;
        return Ok((t.course_id.clone(), t.course_work_id.clone()));
    }
    if r.starts_with("http") {
        return parse_classroom_url(r).context("Not a recognized Classroom assignment link");
    }
    if let Some((c, w)) = r.split_once('/') {
        return Ok((c.into(), w.into()));
    }
    bail!(
        "Unrecognized assignment '{r}'. Use a number from `class list`, a Classroom link or <courseId>/<taskId>"
    )
}

pub fn pick(web: &Web) -> Result<(String, String)> {
    let rows = fetch_tasks(
        web,
        &ListOpts {
            course: None,
            done: false,
            old: false,
        },
    )?;
    if rows.is_empty() {
        bail!("No assignments to do 🎉");
    }
    let items: Vec<String> = rows
        .iter()
        .map(|r| {
            format!(
                "{} — {}  ({})",
                r.course.name,
                r.task.title,
                fmt_due(r.task.due).0
            )
        })
        .collect();
    let idx = dialoguer::FuzzySelect::new()
        .with_prompt("Choose an assignment")
        .items(&items)
        .default(0)
        .interact_opt()?
        .context("Cancelled")?;
    Ok((rows[idx].course.id.clone(), rows[idx].task.id.clone()))
}

pub fn list(ctx: &Ctx, opts: ListOpts) -> Result<()> {
    let web = ctx.web()?;
    let rows = fetch_tasks(&web, &opts)?;
    save_cache(&web.email, &rows);
    if rows.is_empty() {
        println!("No assignments to do 🎉");
        return Ok(());
    }
    let mut t = Table::new();
    t.load_preset(presets::NOTHING)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(
            ["#", "Course", "Assignment", "Due", "Status"]
                .map(|h| Cell::new(h).fg(Color::DarkGrey)),
        );
    let now = Local::now();
    for (i, r) in rows.iter().enumerate() {
        let (due, color) = fmt_due(r.task.due);
        let mut due_cell = Cell::new(if r.task.done {
            due.replace("overdue ", "")
        } else {
            due
        });
        if let (Some(c), false) = (color, r.task.done) {
            due_cell = due_cell.fg(c);
        }
        let (status, status_color) = if r.task.done {
            ("turned in", Color::Green)
        } else if r.task.due.is_some_and(|d| d < now) {
            ("missing", Color::Red)
        } else {
            ("assigned", Color::Reset)
        };
        t.add_row(vec![
            Cell::new(i + 1).fg(Color::Cyan),
            Cell::new(&r.course.name),
            Cell::new(&r.task.title),
            due_cell,
            Cell::new(status).fg(status_color),
        ]);
    }
    println!("{t}");
    let mut footer = format!(
        "\n{} assignments · {} · download: class get <#>",
        rows.len(),
        web.email
    );
    if !opts.old {
        footer.push_str(&format!(" · older than {OLD_AFTER_DAYS} days: --old"));
    }
    println!("{}", style(footer).dim());
    Ok(())
}

pub fn courses(ctx: &Ctx) -> Result<()> {
    let web = ctx.web()?;
    let mut t = Table::new();
    t.load_preset(presets::NOTHING)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(["Course", "Section", "ID"].map(|h| Cell::new(h).fg(Color::DarkGrey)));
    for c in spin("Fetching courses", || web.courses())? {
        t.add_row(vec![
            Cell::new(&c.name),
            Cell::new(c.section.as_deref().unwrap_or("")),
            Cell::new(&c.id).fg(Color::DarkGrey),
        ]);
    }
    println!("{t}");
    Ok(())
}

pub fn open(ctx: &Ctx, task: Option<&str>) -> Result<()> {
    let (course_id, task_id, email) = match task {
        Some(r) => {
            let email = ctx.session(None)?.email;
            let (c, w) = resolve_ref(&email, r)?;
            (c, w, email)
        }
        None => {
            let root = workspace::find_root(&std::env::current_dir()?)?;
            let meta = workspace::load(&root)?;
            (meta.course_id, meta.course_work_id, meta.account)
        }
    };
    let link = with_authuser(&task_link(&course_id, &task_id), &email);
    println!("{link}");
    open::that_detached(&link)?;
    Ok(())
}

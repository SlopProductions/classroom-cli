use super::tasks::{pick, resolve_ref};
use crate::Ctx;
use crate::classroom::MaterialKind;
use crate::progress::{Spinner, spin};
use crate::task_md;
use crate::util::folder_for;
use crate::web::Web;
use crate::workspace::{self, GeneratedFile, TASK_FILE, TaskMeta};
use anyhow::{Context, Result, bail};
use console::style;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub struct GetArgs {
    pub task: Option<String>,
    pub dir: Option<PathBuf>,
    pub no_materials: bool,
    pub force: bool,
    pub quiet: bool,
}

macro_rules! info {
    ($q:expr, $($arg:tt)*) => { if !$q { eprintln!($($arg)*); } };
}

pub fn link(ctx: &Ctx, task: Option<&str>, path: Option<PathBuf>, force: bool) -> Result<()> {
    let root = match path {
        Some(p) => p,
        None => std::env::current_dir()?,
    }
    .canonicalize()?;
    if let Ok(m) = workspace::load(&root)
        && !force
    {
        bail!(
            "{} is already linked to '{}' ({}). Use --force to change it.",
            root.display(),
            m.title,
            m.course_name
        );
    }
    let web = ctx.web()?;
    let (course_id, cw_id) = match task {
        Some(r) => resolve_ref(&web.email, r)?,
        None => pick(&web)?,
    };
    let (course, cw) = spin("Fetching assignment", || -> Result<_> {
        Ok((web.course(&course_id)?, web.task(&course_id, &cw_id)?))
    })?;
    let meta = TaskMeta {
        version: 1,
        account: web.email.clone(),
        course_id: course.id.clone(),
        course_name: course.name.clone(),
        course_work_id: cw.id.clone(),
        title: cw.title.clone(),
        alternate_link: Some(cw.link()),
        due: cw.due.map(|d| d.to_rfc3339()),
        downloaded_at: chrono::Local::now().to_rfc3339(),
        generated: vec![],
        last_submission: None,
    };
    workspace::save(&root, &meta)?;
    eprintln!(
        "{} {} → {} — {}",
        style("✓").green(),
        root.display(),
        course.name,
        style(&cw.title).bold()
    );
    eprintln!(
        "{}",
        style("Preview: class submit -n, turn in: class submit").dim()
    );
    Ok(())
}

pub fn run(ctx: &Ctx, args: GetArgs) -> Result<()> {
    let web = ctx.web()?;
    let (course_id, cw_id) = match &args.task {
        Some(r) => resolve_ref(&web.email, r)?,
        None => pick(&web)?,
    };
    let (course, cw, materials, sub) = spin("Fetching assignment", || -> Result<_> {
        let course = web.course(&course_id)?;
        let cw = web.task(&course_id, &cw_id)?;
        let materials = web.materials(&course_id, &cw_id);
        let sub = web.submission(&course_id, &cw_id).ok();
        Ok((course, cw, materials, sub))
    })?;
    let materials = materials.unwrap_or_else(|e| {
        eprintln!(
            "{} could not fetch the list of materials: {e:#}",
            style("Warning:").yellow()
        );
        vec![]
    });

    let base = match args.dir.clone().or_else(|| ctx.cfg.default_dir()) {
        Some(d) => d,
        None => std::env::current_dir()?,
    };
    let mut root = base.join(folder_for(&ctx.cfg.folder_template, &course, &cw));
    if workspace::load(&root).is_ok_and(|m| m.course_work_id != cw.id) {
        let suffix = &cw.id[cw.id.len().saturating_sub(4)..];
        let name = root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        root.set_file_name(format!("{name}-{suffix}"));
    }

    let old_meta = if root.join(workspace::META_FILE).is_file() {
        let m = workspace::load(&root)?;
        if m.course_work_id != cw.id {
            bail!(
                "{} already contains a different assignment ('{}')",
                root.display(),
                m.title
            );
        }
        if !args.force {
            if args.quiet {
                println!("{}", root.display());
                return Ok(());
            }
            bail!(
                "Assignment already downloaded to {}\nUse --force to refresh TASK.md and materials (your files will not be touched).",
                root.display()
            );
        }
        Some(m)
    } else {
        if root.exists() && fs::read_dir(&root)?.next().is_some() && !args.force {
            bail!(
                "Folder {} already exists and is not empty (use --force to use it anyway)",
                root.display()
            );
        }
        None
    };
    fs::create_dir_all(&root)?;
    info!(
        args.quiet,
        "{} {} — {}",
        style("↓").cyan(),
        course.name,
        style(&cw.title).bold()
    );

    let old_generated: HashMap<String, GeneratedFile> = old_meta
        .as_ref()
        .map(|m| {
            m.generated
                .iter()
                .map(|g| (g.path.clone(), g.clone()))
                .collect()
        })
        .unwrap_or_default();

    let mut generated = vec![];
    let mut local = HashMap::new();
    if ctx.cfg.download_materials && !args.no_materials {
        let mut used = HashSet::new();
        let spinner = Spinner::new("Downloading materials");
        let say = |line: String| {
            if !args.quiet {
                spinner.println(line);
            }
        };
        for m in &materials {
            let MaterialKind::Drive { id, .. } = &m.kind else {
                continue;
            };
            spinner.set(format!("Downloading {}", m.title));
            match download_material(&web, id, &root, &mut used, &old_generated, &say) {
                Ok(Some(g)) => {
                    say(format!("  {} {}", style("✓").green(), g.path));
                    local.insert(id.clone(), g.path.clone());
                    generated.push(g);
                }
                Ok(None) => {}
                Err(e) => {
                    say(format!("  {} {}: {e:#}", style("✗").red(), m.title));
                }
            }
        }
    }

    let md = task_md::render(&course.name, &cw, &materials, sub.as_ref(), &local);
    fs::write(root.join(TASK_FILE), &md)?;
    generated.insert(
        0,
        GeneratedFile {
            path: TASK_FILE.into(),
            sha256: workspace::sha256_bytes(md.as_bytes()),
        },
    );

    let meta = TaskMeta {
        version: 1,
        account: web.email.clone(),
        course_id: course.id.clone(),
        course_name: course.name.clone(),
        course_work_id: cw.id.clone(),
        title: cw.title.clone(),
        alternate_link: Some(cw.link()),
        due: cw.due.map(|d| d.to_rfc3339()),
        downloaded_at: chrono::Local::now().to_rfc3339(),
        generated,
        last_submission: old_meta.and_then(|m| m.last_submission),
    };
    workspace::save(&root, &meta)?;

    if args.quiet {
        println!("{}", root.display());
    } else {
        eprintln!("\n{} {}", style("Done:").green().bold(), root.display());
        eprintln!(
            "{}",
            style("When finished: class submit (in this folder)").dim()
        );
    }
    Ok(())
}

const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
const XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
const PPTX: &str = "application/vnd.openxmlformats-officedocument.presentationml.presentation";

fn target(meta: &Value) -> Option<(String, Option<&'static str>)> {
    let name = meta["title"].as_str().unwrap_or("file");
    let export = |ext: &str, mime| Some((format!("{name}.{ext}"), Some(mime)));
    match meta["mimeType"].as_str().unwrap_or("") {
        "application/vnd.google-apps.document" => export("docx", DOCX),
        "application/vnd.google-apps.spreadsheet" => export("xlsx", XLSX),
        "application/vnd.google-apps.presentation" => export("pptx", PPTX),
        "application/vnd.google-apps.drawing" => export("png", "image/png"),
        m if m.starts_with("application/vnd.google-apps.") => None,
        _ => Some((name.to_string(), None)),
    }
}

fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let s = s.trim().trim_start_matches('.').to_string();
    if s.is_empty() { "file".into() } else { s }
}

fn unique(name: String, used: &mut HashSet<String>) -> String {
    let mut candidate = name.clone();
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.clone(), String::new()),
    };
    let mut i = 2;
    while used.contains(&candidate) {
        candidate = format!("{stem} ({i}){ext}");
        i += 1;
    }
    used.insert(candidate.clone());
    candidate
}

fn download_material(
    web: &Web,
    id: &str,
    root: &Path,
    used: &mut HashSet<String>,
    old: &HashMap<String, GeneratedFile>,
    say: &dyn Fn(String),
) -> Result<Option<GeneratedFile>> {
    let meta = web.drive_meta(id)?;
    let Some((name, export)) = target(&meta) else {
        return Ok(None);
    };
    let rel = format!("materials/{}", unique(sanitize(&name), used));
    let full = root.join(&rel);

    if let Some(g) = old.get(&rel)
        && full.is_file()
        && workspace::sha256_file(&full)? != g.sha256
    {
        say(format!(
            "  {} {rel} — modified by you, not overwriting",
            style("•").yellow()
        ));
        return Ok(Some(g.clone()));
    }
    let url = match export {
        Some(mime) => meta["exportLinks"][mime]
            .as_str()
            .context("Drive does not offer an export for this file")?
            .to_string(),
        None => web.drive_download_url(id),
    };
    let (bytes, ctype) = web.download(&url)?;
    let expects_html = name.ends_with(".html") || name.ends_with(".htm");
    if ctype.starts_with("text/html") && !expects_html {
        bail!("Drive returned a page instead of the file (no access?)");
    }
    fs::create_dir_all(root.join("materials"))?;
    fs::write(&full, &bytes)?;
    Ok(Some(GeneratedFile {
        path: rel,
        sha256: workspace::sha256_bytes(&bytes),
    }))
}

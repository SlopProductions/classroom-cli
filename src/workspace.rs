use anyhow::{Context, Result, bail};
use ignore::WalkBuilder;
use ignore::gitignore::GitignoreBuilder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const META_DIR: &str = ".class";
pub const META_FILE: &str = ".class/task.json";
pub const TASK_FILE: &str = "TASK.md";
pub const IGNORE_FILE: &str = ".classignore";

const DEFAULT_IGNORES: &[&str] = &[
    "/.class/",
    "/.classignore",
    "/TASK.md",
    ".git/",
    ".DS_Store",
    "._*",
    "__MACOSX/",
    "Thumbs.db",
    "desktop.ini",
    "~$*",
    ".~lock.*#",
    "*.swp",
    "node_modules/",
    "__pycache__/",
    ".venv/",
    "venv/",
    "target/",
    ".idea/",
    ".vscode/",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedFile {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmissionRecord {
    pub at: String,
    pub file_name: String,
    pub drive_file_id: String,
    pub drive_link: Option<String>,
    pub attached: bool,
    pub turned_in: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskMeta {
    pub version: u32,
    pub account: String,
    pub course_id: String,
    pub course_name: String,
    pub course_work_id: String,
    pub title: String,
    pub alternate_link: Option<String>,
    pub due: Option<String>,
    pub downloaded_at: String,
    #[serde(default)]
    pub generated: Vec<GeneratedFile>,
    #[serde(default)]
    pub last_submission: Option<SubmissionRecord>,
}

pub fn find_root(start: &Path) -> Result<PathBuf> {
    let start = start
        .canonicalize()
        .with_context(|| format!("Path {} does not exist", start.display()))?;
    for dir in start.ancestors() {
        if dir.join(META_FILE).is_file() {
            return Ok(dir.to_path_buf());
        }
    }
    bail!(
        "{} is not an assignment folder (missing {META_FILE}). Download an assignment with: class get",
        start.display()
    )
}

pub fn load(root: &Path) -> Result<TaskMeta> {
    let data = fs::read(root.join(META_FILE))?;
    serde_json::from_slice(&data).context("Corrupted .class/task.json")
}

pub fn save(root: &Path, meta: &TaskMeta) -> Result<()> {
    fs::create_dir_all(root.join(META_DIR))?;
    fs::write(root.join(META_FILE), serde_json::to_vec_pretty(meta)?)?;
    Ok(())
}

pub fn sha256_bytes(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

#[derive(Debug, Default)]
pub struct Collected {
    pub include: Vec<String>,
    pub skipped_generated: Vec<String>,
    pub modified_generated: Vec<String>,
}

pub fn collect(root: &Path, meta: &TaskMeta) -> Result<Collected> {
    let mut gb = GitignoreBuilder::new(root);
    for pat in DEFAULT_IGNORES {
        gb.add_line(None, pat)?;
    }
    let defaults = gb.build()?;
    let generated: HashMap<&str, &GeneratedFile> = meta
        .generated
        .iter()
        .map(|g| (g.path.as_str(), g))
        .collect();

    let base = root.to_path_buf();
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .parents(false)
        .ignore(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .require_git(false)
        .add_custom_ignore_filename(IGNORE_FILE)
        .sort_by_file_name(|a, b| a.cmp(b))
        .filter_entry(move |e| {
            let rel = e.path().strip_prefix(&base).unwrap_or(e.path());
            let is_dir = e.file_type().is_some_and(|t| t.is_dir());
            rel.as_os_str().is_empty() || !defaults.matched(rel, is_dir).is_ignore()
        })
        .build();

    let mut out = Collected::default();
    for entry in walker {
        let entry = entry?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let rel = rel_string(entry.path().strip_prefix(root)?);
        match generated.get(rel.as_str()) {
            Some(g) if sha256_file(entry.path())? == g.sha256 => out.skipped_generated.push(rel),
            Some(_) => {
                out.modified_generated.push(rel.clone());
                out.include.push(rel);
            }
            None => out.include.push(rel),
        }
    }
    Ok(out)
}

fn rel_string(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

pub fn make_zip(root: &Path, files: &[String], out: &Path) -> Result<()> {
    let mut zw = zip::ZipWriter::new(File::create(out)?);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    for rel in files {
        zw.start_file(rel.as_str(), opts)?;
        let mut f = File::open(root.join(rel))?;
        std::io::copy(&mut f, &mut zw)?;
    }
    zw.finish()?.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(generated: Vec<GeneratedFile>) -> TaskMeta {
        TaskMeta {
            version: 1,
            account: "a@b.c".into(),
            course_id: "1".into(),
            course_name: "c".into(),
            course_work_id: "2".into(),
            title: "t".into(),
            alternate_link: None,
            due: None,
            downloaded_at: String::new(),
            generated,
            last_submission: None,
        }
    }

    #[test]
    fn collects_only_user_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join(".class")).unwrap();
        fs::create_dir_all(root.join("materials")).unwrap();
        fs::create_dir_all(root.join("src/node_modules/x")).unwrap();
        fs::write(root.join(".class/task.json"), "{}").unwrap();
        fs::write(root.join("TASK.md"), "# t").unwrap();
        fs::write(root.join("materials/a.pdf"), "A").unwrap();
        fs::write(root.join("materials/worksheet.docx"), "changed").unwrap();
        fs::write(root.join("src/main.py"), "print()").unwrap();
        fs::write(root.join("src/node_modules/x/y.js"), "").unwrap();
        fs::write(root.join("~$report.docx"), "").unwrap();
        fs::write(root.join("report.docx"), "x").unwrap();
        fs::write(root.join("notes.txt"), "x").unwrap();
        fs::write(root.join(".classignore"), "notes.txt\n").unwrap();
        fs::create_dir_all(root.join("app/dist")).unwrap();
        fs::write(root.join("app/.gitignore"), "dist\n").unwrap();
        fs::write(root.join("app/dist/bundle.js"), "x").unwrap();
        fs::write(root.join("app/index.html"), "x").unwrap();

        let m = meta(vec![
            GeneratedFile {
                path: "materials/a.pdf".into(),
                sha256: sha256_bytes(b"A"),
            },
            GeneratedFile {
                path: "materials/worksheet.docx".into(),
                sha256: sha256_bytes(b"orig"),
            },
        ]);
        let c = collect(root, &m).unwrap();
        assert_eq!(
            c.include,
            vec![
                "app/.gitignore",
                "app/index.html",
                "materials/worksheet.docx",
                "report.docx",
                "src/main.py"
            ]
        );
        assert_eq!(c.skipped_generated, vec!["materials/a.pdf"]);
        assert_eq!(c.modified_generated, vec!["materials/worksheet.docx"]);

        let zip_path = root.join("../out.zip");
        make_zip(root, &c.include, &zip_path).unwrap();
        let z = zip::ZipArchive::new(File::open(&zip_path).unwrap()).unwrap();
        assert_eq!(z.len(), 5);
        fs::remove_file(zip_path).unwrap();
    }
}

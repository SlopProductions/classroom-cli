mod browser;
mod classroom;
mod commands;
mod config;
mod progress;
mod session;
mod task_md;
mod util;
mod web;
mod workspace;

use anyhow::{Result, bail};
use clap::{CommandFactory, Parser, Subcommand};
use commands::{account, dev, done, get, submit, tasks};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "class",
    version,
    about = "Google Classroom from the terminal",
    propagate_version = true
)]
struct Cli {
    #[arg(
        short,
        long,
        global = true,
        help = "Google account to use (email or part of it), defaults to the active one"
    )]
    account: Option<String>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    #[command(subcommand, about = "Manage Google accounts (log in, switch)")]
    Auth(AuthCmd),

    #[command(
        visible_alias = "ls",
        about = "List assignments, by default only pending ones, sorted by due date"
    )]
    List {
        #[arg(long, help = "Also show turned in and returned assignments")]
        all: bool,
        #[arg(long, help = "Also show assignments more than 30 days overdue")]
        old: bool,
        #[arg(short, long, help = "Filter by course name (substring)")]
        course: Option<String>,
    },

    #[command(about = "Download an assignment into a new folder (TASK.md + materials)")]
    Get {
        #[arg(
            help = "Number from `class list`, assignment link or <courseId>/<courseWorkId>; without an argument: pick from a list"
        )]
        task: Option<String>,
        #[arg(short, long, help = "Directory to create the assignment folder in")]
        dir: Option<PathBuf>,
        #[arg(long, help = "Do not download teacher attachments")]
        no_materials: bool,
        #[arg(short, long, help = "Refresh an existing assignment folder")]
        force: bool,
        #[arg(
            short,
            long,
            help = "Print only the folder path, e.g. cd \"$(class get 3 -q)\""
        )]
        quiet: bool,
    },

    #[command(
        about = "Link an existing folder to a Classroom assignment so it can be turned in with class submit"
    )]
    Link {
        #[arg(
            help = "Number from `class list`, assignment link or <courseId>/<taskId>; without an argument: pick from a list"
        )]
        task: Option<String>,
        #[arg(short, long, help = "Folder (defaults to the current one)")]
        path: Option<PathBuf>,
        #[arg(
            short,
            long,
            help = "Change the assignment if the folder is already linked"
        )]
        force: bool,
    },

    #[command(
        about = "Turn in the assignment from the current folder (1 file: that file, more: a zip)"
    )]
    Submit {
        #[arg(help = "Assignment folder (defaults to the current one)")]
        path: Option<PathBuf>,
        #[arg(short = 'n', long, help = "Only show what would be uploaded")]
        dry_run: bool,
        #[arg(short, long, help = "Do not ask for confirmation")]
        yes: bool,
        #[arg(long, help = "Only attach the file, do not turn in")]
        no_turn_in: bool,
    },

    #[command(
        about = "Mark assignments as done without attachments (one, or all older than N days)"
    )]
    Done {
        #[arg(
            help = "Number from `class list`, assignment link or <courseId>/<taskId>; without it: all old pending assignments"
        )]
        task: Option<String>,
        #[arg(
            long,
            value_name = "DAYS",
            default_value_t = 30,
            help = "Only assignments due (or posted, if undated) more than DAYS ago"
        )]
        older_than: u32,
        #[arg(short, long, help = "Filter by course name (substring)")]
        course: Option<String>,
        #[arg(short = 'n', long, help = "Only show what would be marked")]
        dry_run: bool,
        #[arg(short, long, help = "Do not ask for confirmation")]
        yes: bool,
    },

    #[command(about = "Unsubmit the assignment from the current folder")]
    Unsubmit {
        #[arg(help = "Assignment folder (defaults to the current one)")]
        path: Option<PathBuf>,
        #[arg(short, long, help = "Do not ask for confirmation")]
        yes: bool,
    },

    #[command(
        about = "Show the status of the assignment in the current folder and the files to turn in"
    )]
    Status {
        #[arg(help = "Assignment folder (defaults to the current one)")]
        path: Option<PathBuf>,
    },

    #[command(about = "Open the assignment in the browser (current folder or number/link)")]
    Open { task: Option<String> },

    #[command(about = "List courses")]
    Courses,

    #[command(about = "Configuration (without arguments: show)")]
    Config {
        #[command(subcommand)]
        cmd: Option<ConfigCmd>,
    },

    #[command(subcommand, hide = true, about = "Developer tools")]
    Dev(DevCmd),

    #[command(about = "Generate a shell completion script, e.g. class completions zsh")]
    Completions { shell: clap_complete::Shell },
}

#[derive(Subcommand)]
enum AuthCmd {
    #[command(about = "Log in to a Google account in a browser window (Chrome, Helium, Brave...)")]
    Login {
        #[arg(
            long,
            value_name = "BROWSER",
            help = "Import the session from a browser you are already logged in to: chrome, chromium, helium, brave, edge"
        )]
        from: Option<String>,
    },
    #[command(visible_alias = "ls", about = "List logged in accounts (* = active)")]
    List,
    #[command(about = "Set the active account")]
    Use { account: String },
    #[command(about = "Log out and remove an account")]
    Logout { account: String },
}

#[derive(Subcommand)]
enum ConfigCmd {
    #[command(about = "Show the configuration")]
    Show,
    #[command(
        about = "Set a value: default_dir, folder_template, download_materials, active_account, browser"
    )]
    Set { key: String, value: String },
    #[command(about = "Print the configuration directory path")]
    Path,
}

#[derive(Subcommand)]
enum DevCmd {
    #[command(about = "Record Classroom network traffic from the logged in browser to a file")]
    Capture,
}

pub struct Ctx {
    pub cfg: config::Config,
    pub account: Option<String>,
}

impl Ctx {
    pub fn session(&self, preferred: Option<&str>) -> Result<session::Session> {
        let query = self
            .account
            .clone()
            .or_else(|| preferred.map(String::from))
            .or_else(|| self.cfg.active_account.clone());
        if let Some(q) = query {
            return session::find(&q);
        }
        let mut all = session::list()?;
        match all.len() {
            0 => bail!("No account logged in. Log in with: class auth login"),
            1 => Ok(all.remove(0)),
            _ => bail!("Multiple accounts found, choose the default one: class auth use <email>"),
        }
    }

    pub fn web(&self) -> Result<web::Web> {
        self.web_for(None)
    }

    pub fn web_for(&self, preferred: Option<&str>) -> Result<web::Web> {
        let session = self.session(preferred)?;
        let spinner = progress::Spinner::new("Connecting to Classroom");
        web::Web::connect(session, &|msg| spinner.set(msg.to_string()))
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{} {e:#}", console::style("Error:").red().bold());
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let mut ctx = Ctx {
        cfg: config::Config::load()?,
        account: cli.account,
    };

    match cli.cmd {
        Cmd::Auth(cmd) => match cmd {
            AuthCmd::Login { from } => account::login(&mut ctx, from.as_deref()),
            AuthCmd::List => account::list(&ctx),
            AuthCmd::Use { account: a } => account::use_account(&mut ctx, &a),
            AuthCmd::Logout { account: a } => account::logout(&mut ctx, &a),
        },
        Cmd::List { all, old, course } => tasks::list(
            &ctx,
            tasks::ListOpts {
                course: course.as_deref(),
                done: all,
                old,
            },
        ),
        Cmd::Get {
            task,
            dir,
            no_materials,
            force,
            quiet,
        } => get::run(
            &ctx,
            get::GetArgs {
                task,
                dir,
                no_materials,
                force,
                quiet,
            },
        ),
        Cmd::Submit {
            path,
            dry_run,
            yes,
            no_turn_in,
        } => submit::submit(
            &ctx,
            submit::SubmitArgs {
                path,
                dry_run,
                yes,
                no_turn_in,
            },
        ),
        Cmd::Link { task, path, force } => get::link(&ctx, task.as_deref(), path, force),
        Cmd::Done {
            task,
            older_than,
            course,
            dry_run,
            yes,
        } => done::run(
            &ctx,
            done::DoneArgs {
                task,
                older_than,
                course,
                dry_run,
                yes,
            },
        ),
        Cmd::Unsubmit { path, yes } => submit::unsubmit(&ctx, path, yes),
        Cmd::Status { path } => submit::status(&ctx, path),
        Cmd::Open { task } => tasks::open(&ctx, task.as_deref()),
        Cmd::Courses => tasks::courses(&ctx),
        Cmd::Config { cmd } => match cmd.unwrap_or(ConfigCmd::Show) {
            ConfigCmd::Show => account::config_show(&ctx.cfg),
            ConfigCmd::Set { key, value } => account::config_set(&mut ctx.cfg, &key, &value),
            ConfigCmd::Path => {
                println!("{}", config::config_dir().display());
                Ok(())
            }
        },
        Cmd::Dev(DevCmd::Capture) => dev::capture(&ctx),
        Cmd::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "class", &mut std::io::stdout());
            Ok(())
        }
    }
}

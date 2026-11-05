use crate::Ctx;
use crate::browser;
use crate::config;
use crate::progress::spin;
use crate::session;
use crate::web::Web;
use anyhow::{Result, bail};
use console::style;

struct Report {
    failed: usize,
}

impl Report {
    fn ok(&self, what: &str, detail: impl AsRef<str>) {
        println!(
            "{} {what:<16} {}",
            style("✓").green(),
            style(detail.as_ref()).dim()
        );
    }

    fn fail(&mut self, what: &str, detail: impl AsRef<str>) {
        self.failed += 1;
        println!("{} {what:<16} {}", style("✗").red(), detail.as_ref());
    }
}

pub fn run(ctx: &Ctx) -> Result<()> {
    let mut r = Report { failed: 0 };
    r.ok("version", env!("CARGO_PKG_VERSION"));
    r.ok("config", config::config_dir().display().to_string());

    let installed = browser::installed();
    if installed.is_empty() {
        r.fail(
            "browser",
            "no Chromium-based browser found, use --browser <path> when logging in",
        );
    } else {
        let names: Vec<_> = installed.iter().map(|(k, _)| k.label).collect();
        r.ok("browsers", names.join(", "));
    }

    let sess = match ctx.session(None) {
        Ok(s) => s,
        Err(e) => {
            r.fail("account", format!("{e:#}"));
            bail!("{} check(s) failed", r.failed);
        }
    };
    r.ok(
        "account",
        format!(
            "{} (session saved {})",
            sess.email,
            &sess.saved_at[..10.min(sess.saved_at.len())]
        ),
    );
    match sess.browser_binary() {
        Ok(b) => r.ok("session browser", b.display().to_string()),
        Err(e) => r.fail("session browser", format!("{e:#}")),
    }
    let profile = session::profile_dir(&sess.email);
    if profile.is_dir() {
        r.ok("profile", profile.display().to_string());
    } else {
        r.fail(
            "profile",
            "missing, the session cannot be refreshed. Run: class auth login",
        );
    }

    let web = match spin("Connecting to Classroom", || {
        Web::connect(sess.clone(), &|_| {})
    }) {
        Ok(w) => {
            r.ok(
                "classroom",
                format!("connected as {} (/u/{})", w.email, w.authuser),
            );
            w
        }
        Err(e) => {
            r.fail("classroom", format!("{e:#}"));
            bail!("{} check(s) failed", r.failed);
        }
    };
    match spin("Checking courses", || web.courses()) {
        Ok(c) => r.ok("courses", format!("{} courses", c.len())),
        Err(e) => r.fail("courses", format!("{e:#}")),
    }
    match spin("Checking Google Drive", || web.check_drive()) {
        Ok(()) => r.ok("drive", "uploads and downloads available"),
        Err(e) => r.fail("drive", format!("{e:#}")),
    }

    if r.failed > 0 {
        bail!("{} check(s) failed", r.failed);
    }
    println!("\n{}", style("Everything looks good").green());
    Ok(())
}

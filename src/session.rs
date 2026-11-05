use crate::browser::{self, Browser, Cdp, is_closed};
use crate::config;
use crate::progress::{Spinner, spin};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

const LOGIN_URL: &str =
    "https://accounts.google.com/ServiceLogin?continue=https%3A%2F%2Fclassroom.google.com%2F&hl=en";
#[cfg(target_os = "macos")]
pub const CHROME_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Safari/537.36";
#[cfg(not(target_os = "macos"))]
pub const CHROME_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Safari/537.36";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    #[serde(default)]
    pub secure: bool,
    #[serde(default)]
    pub http_only: bool,
    #[serde(default)]
    pub expires: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub email: String,
    pub cookies: Vec<Cookie>,
    pub saved_at: String,
    #[serde(default)]
    pub browser: Option<String>,
}

impl Session {
    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.cookies
            .iter()
            .find(|c| c.name == name && c.domain.ends_with("google.com"))
            .map(|c| c.value.as_str())
    }

    pub fn browser_binary(&self) -> Result<PathBuf> {
        match &self.browser {
            Some(b) if Path::new(b).is_file() => Ok(b.into()),
            other => browser::resolve(other.as_deref()),
        }
    }

    pub fn cookie_header(&self, url: &str) -> String {
        let Ok(u) = url::Url::parse(url) else {
            return String::new();
        };
        let host = u.host_str().unwrap_or("");
        let path = u.path();
        let now = chrono::Utc::now().timestamp() as f64;
        self.cookies
            .iter()
            .filter(|c| {
                let domain_ok = match c.domain.strip_prefix('.') {
                    Some(d) => host == d || host.ends_with(&c.domain),
                    None => host == c.domain,
                };
                domain_ok && path.starts_with(&c.path) && (c.expires <= 0.0 || c.expires > now)
            })
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

fn sessions_dir() -> PathBuf {
    config::config_dir().join("sessions")
}

fn session_path(email: &str) -> PathBuf {
    sessions_dir().join(format!("{email}.json"))
}

fn profiles_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(config::config_dir)
        .join("class")
        .join("chrome")
}

pub fn profile_dir(email: &str) -> PathBuf {
    profiles_dir().join(email)
}

pub fn save(s: &Session) -> Result<()> {
    fs::create_dir_all(sessions_dir())?;
    config::write_private(&session_path(&s.email), &serde_json::to_vec_pretty(s)?)
}

pub fn list() -> Result<Vec<Session>> {
    let dir = sessions_dir();
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut out: Vec<Session> = vec![];
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "json") {
            out.push(serde_json::from_slice(&fs::read(&path)?)?);
        }
    }
    out.sort_by(|a, b| a.email.cmp(&b.email));
    Ok(out)
}

pub fn find(query: &str) -> Result<Session> {
    let all = list()?;
    if let Some(s) = all.iter().find(|s| s.email == query) {
        return Ok(s.clone());
    }
    let q = query.to_lowercase();
    let matches: Vec<_> = all
        .iter()
        .filter(|s| s.email.to_lowercase().contains(&q))
        .collect();
    match matches.as_slice() {
        [one] => Ok((*one).clone()),
        [] => bail!("No logged in account matches '{query}'. See: class auth list"),
        _ => bail!("'{query}' matches multiple accounts, use the full email"),
    }
}

pub fn delete(email: &str) -> Result<()> {
    let _ = fs::remove_file(session_path(email));
    let profile = profile_dir(email);
    if profile.exists() {
        fs::remove_dir_all(profile)?;
    }
    Ok(())
}

pub fn read_cookies(cdp: &mut Cdp) -> Result<Vec<Cookie>> {
    let r = cdp.call("Storage.getCookies", json!({}), None)?;
    let mut out = vec![];
    for c in r["cookies"].as_array().into_iter().flatten() {
        let domain = c["domain"].as_str().unwrap_or("");
        if !domain.contains("google") {
            continue;
        }
        out.push(Cookie {
            name: c["name"].as_str().unwrap_or("").into(),
            value: c["value"].as_str().unwrap_or("").into(),
            domain: domain.into(),
            path: c["path"].as_str().unwrap_or("/").into(),
            secure: c["secure"].as_bool().unwrap_or(false),
            http_only: c["httpOnly"].as_bool().unwrap_or(false),
            expires: c["expires"].as_f64().unwrap_or(-1.0),
        });
    }
    Ok(out)
}

fn has_cookie(cookies: &[Cookie], name: &str) -> bool {
    cookies
        .iter()
        .any(|c| c.name == name && c.domain.ends_with("google.com"))
}

fn wait_for_login(cdp: &mut Cdp, timeout: Option<Duration>) -> Result<Option<Vec<Cookie>>> {
    let start = Instant::now();
    loop {
        sleep(Duration::from_secs(1));
        if timeout.is_some_and(|t| start.elapsed() > t) {
            return Ok(None);
        }
        let targets = cdp.call("Target.getTargets", json!({}), None)?;
        let pages: Vec<&Value> = targets["targetInfos"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| t["type"] == "page")
            .collect();
        if pages.is_empty() {
            bail!("Browser window was closed before logging in");
        }
        let on_classroom = pages.iter().any(|t| {
            t["url"]
                .as_str()
                .is_some_and(|u| u.starts_with("https://classroom.google.com/"))
        });
        if on_classroom {
            let cookies = read_cookies(cdp)?;
            if has_cookie(&cookies, "SAPISID") && has_cookie(&cookies, "__Secure-1PSID") {
                return Ok(Some(cookies));
            }
        }
    }
}

fn find_emails(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) if s.contains('@') && !s.contains(' ') && s.contains('.') => {
            if !out.contains(s) {
                out.push(s.clone())
            }
        }
        Value::Array(a) => a.iter().for_each(|x| find_emails(x, out)),
        Value::Object(o) => o.values().for_each(|x| find_emails(x, out)),
        _ => {}
    }
}

fn detect_emails(session: &Session) -> Vec<String> {
    let url =
        "https://accounts.google.com/ListAccounts?gpsia=1&source=ChromiumBrowser&json=standard";
    let Some(text) = reqwest::blocking::Client::new()
        .post(url)
        .header("Content-Length", "0")
        .header("Cookie", session.cookie_header(url))
        .header("User-Agent", CHROME_UA)
        .send()
        .ok()
        .and_then(|r| r.text().ok())
    else {
        return vec![];
    };
    let Some(v) = text
        .find('[')
        .and_then(|i| serde_json::from_str::<Value>(&text[i..]).ok())
    else {
        return vec![];
    };
    let mut emails = vec![];
    find_emails(&v, &mut emails);
    emails
}

fn choose_email(session: &Session, wanted: Option<&str>) -> Result<String> {
    let mut emails = detect_emails(session);
    if let Some(w) = wanted {
        let w = w.to_lowercase();
        if let Some(e) = emails.iter().find(|e| e.to_lowercase().contains(&w)) {
            return Ok(e.clone());
        }
        if w.contains('@') {
            return Ok(w);
        }
        bail!(
            "No account in the browser matches '{w}'. Available: {}",
            emails.join(", ")
        );
    }
    if emails.len() > 1 && !console::Term::stderr().is_term() {
        bail!(
            "The browser has multiple accounts: {}. Choose the school one with -a <email>",
            emails.join(", ")
        );
    }
    match emails.len() {
        0 => Ok(dialoguer::Input::<String>::new()
            .with_prompt("Could not detect the email. Enter the account email")
            .interact_text()?),
        1 => Ok(emails.remove(0)),
        _ => {
            let idx = dialoguer::Select::new()
                .with_prompt(
                    "The browser has multiple Google accounts. Which one is the school account?",
                )
                .items(&emails)
                .default(0)
                .interact()?;
            Ok(emails.remove(idx))
        }
    }
}

fn import_profile(kind: &browser::Kind, dest: &Path) -> Result<()> {
    let src = kind.user_data_dir();
    let local_state = src.join("Local State");
    if !local_state.is_file() {
        bail!("No {} profile found in {}", kind.name, src.display());
    }
    let profile = fs::read(&local_state)
        .ok()
        .and_then(|d| serde_json::from_slice::<Value>(&d).ok())
        .and_then(|v| v["profile"]["last_used"].as_str().map(String::from))
        .unwrap_or_else(|| "Default".into());
    fs::create_dir_all(dest.join("Default"))?;
    fs::copy(&local_state, dest.join("Local State"))?;
    let mut copied = false;
    for rel in [
        "Cookies",
        "Cookies-journal",
        "Network/Cookies",
        "Network/Cookies-journal",
    ] {
        let from = src.join(&profile).join(rel);
        if from.is_file() {
            let to = dest.join("Default").join(rel);
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&from, &to)?;
            copied |= !rel.ends_with("-journal");
        }
    }
    if !copied {
        bail!(
            "The {} profile has no cookies file ({})",
            kind.name,
            src.join(&profile).display()
        );
    }
    Ok(())
}

fn interactive_login(binary: &Path, profile: &Path) -> Result<Vec<Cookie>> {
    let mut b = Browser::launch(binary, profile, LOGIN_URL, false)?;
    let spinner = Spinner::new("Log in to your school account in the browser window");
    let res = wait_for_login(&mut b.cdp, None);
    spinner.set("Saving session");
    b.close();
    drop(spinner);
    match res {
        Ok(Some(c)) => Ok(c),
        Ok(None) => bail!("Login failed"),
        Err(e) if is_closed(&e) => bail!("Browser window was closed before logging in"),
        Err(e) => Err(e),
    }
}

fn headless_check(binary: &Path, profile: &Path) -> Result<Option<Vec<Cookie>>> {
    headless_visit(binary, profile, "https://classroom.google.com/")
}

fn headless_visit(binary: &Path, profile: &Path, url: &str) -> Result<Option<Vec<Cookie>>> {
    let mut b = Browser::launch(binary, profile, url, true)?;
    let res = wait_for_login(&mut b.cdp, Some(Duration::from_secs(20)));
    b.close();
    res
}

enum Source {
    Import(&'static browser::Kind, PathBuf),
    Window(PathBuf),
}

fn choose_source(
    cfg_browser: Option<&str>,
    from: Option<&str>,
    browser_arg: Option<&str>,
) -> Result<Source> {
    if let Some(name) = from {
        let kind = browser::kind(name)?;
        let binary = match browser_arg {
            Some(b) => browser::resolve(Some(b))?,
            None => kind.binary()?,
        };
        return Ok(Source::Import(kind, binary));
    }
    if let Some(b) = browser_arg {
        return Ok(Source::Window(browser::resolve(Some(b))?));
    }
    if cfg_browser.is_some()
        || std::env::var_os("CLASS_CHROME").is_some()
        || !console::Term::stderr().is_term()
    {
        return Ok(Source::Window(browser::resolve(cfg_browser)?));
    }
    let installed = browser::installed();
    if installed.is_empty() {
        bail!(
            "No Chromium-based browser found ({}). Install one or pass --browser <path>",
            browser::names()
        );
    }
    let mut options: Vec<(String, Source)> = vec![];
    for (kind, bin) in &installed {
        if kind.has_profile() {
            options.push((
                format!("Use my {} session", kind.label),
                Source::Import(kind, bin.clone()),
            ));
        }
    }
    for (kind, bin) in &installed {
        options.push((
            format!("Log in in a new {} window", kind.label),
            Source::Window(bin.clone()),
        ));
    }
    let labels: Vec<&str> = options.iter().map(|(l, _)| l.as_str()).collect();
    let idx = dialoguer::Select::new()
        .with_prompt("How do you want to log in?")
        .items(&labels)
        .default(0)
        .interact()?;
    Ok(options.swap_remove(idx).1)
}

fn import_or_window(kind: &browser::Kind, binary: &Path, tmp: &Path) -> Result<Vec<Cookie>> {
    if !kind.has_profile() {
        eprintln!(
            "No {} profile found in {}, opening a login window instead",
            kind.label,
            kind.user_data_dir().display()
        );
        return interactive_login(binary, tmp);
    }
    import_profile(kind, tmp)?;
    let found = spin(
        format!("Importing Google session from {}", kind.label),
        || headless_check(binary, tmp),
    )?;
    match found {
        Some(c) => Ok(c),
        None => {
            eprintln!(
                "No active Classroom session in {}, opening a login window instead",
                kind.label
            );
            interactive_login(binary, tmp)
        }
    }
}

pub fn login(
    cfg_browser: Option<&str>,
    from: Option<&str>,
    browser_arg: Option<&str>,
    wanted: Option<&str>,
) -> Result<Session> {
    let source = choose_source(cfg_browser, from, browser_arg)?;
    let tmp = profiles_dir().join(format!("new-{}", rand::random::<u32>()));
    let result = (|| -> Result<(PathBuf, Vec<Cookie>)> {
        match source {
            Source::Import(kind, binary) => {
                let cookies = import_or_window(kind, &binary, &tmp)?;
                Ok((binary, cookies))
            }
            Source::Window(binary) => {
                let cookies = interactive_login(&binary, &tmp)?;
                Ok((binary, cookies))
            }
        }
    })();
    let (binary, cookies) = match result {
        Ok(r) => r,
        Err(e) => {
            let _ = fs::remove_dir_all(&tmp);
            return Err(e);
        }
    };

    let mut session = Session {
        email: String::new(),
        cookies,
        saved_at: chrono::Local::now().to_rfc3339(),
        browser: Some(binary.to_string_lossy().into_owned()),
    };
    session.email = match choose_email(&session, wanted) {
        Ok(e) => e,
        Err(e) => {
            let _ = fs::remove_dir_all(&tmp);
            return Err(e);
        }
    };

    let target = profile_dir(&session.email);
    if target.exists() {
        fs::remove_dir_all(&target)?;
    }
    fs::rename(&tmp, &target).context("Could not save the browser profile")?;
    save(&session)?;
    Ok(session)
}

pub fn refresh(s: &mut Session) -> Result<()> {
    let profile = profile_dir(&s.email);
    if !profile.exists() {
        bail!(
            "No browser profile for {}. Log in again: class auth login",
            s.email
        );
    }
    let url = format!("https://classroom.google.com/?authuser={}", s.email);
    match headless_visit(&s.browser_binary()?, &profile, &url)? {
        Some(cookies) => {
            s.cookies = cookies;
            s.saved_at = chrono::Local::now().to_rfc3339();
            save(s)
        }
        None => bail!(
            "Session for {} expired. Log in again: class auth login",
            s.email
        ),
    }
}

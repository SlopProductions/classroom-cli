use crate::Ctx;
use crate::browser::Browser;
use crate::config;
use crate::session;
use anyhow::{Result, bail};
use console::style;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufWriter, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const MAX_TEXT_BODY: usize = 512 * 1024;
const MAX_HTML_BODY: usize = 4 * 1024 * 1024;
const MAX_POST: usize = 256 * 1024;

const NOISE: &[&str] = &[
    "play.google.com/log",
    "/gen_204",
    "/jserror",
    "/cspreport",
    "gstatic.com",
    "fonts.googleapis.com",
    "signaler-pa.",
    "waa-pa.",
    "ogads-pa.",
    "/_/scs/",
    "apis.google.com/js",
    "accounts.google.com/RotateCookies",
    "googleusercontent.com/a/",
    "lh3.googleusercontent.com",
    "favicon.ico",
];

fn interesting(url: &str, kind: &str) -> bool {
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
        .unwrap_or_default();
    let google = host.ends_with("google.com")
        || host.ends_with("googleapis.com")
        || host.ends_with("googleusercontent.com");
    google
        && matches!(kind, "XHR" | "Fetch" | "Document" | "Other" | "Ping")
        && !NOISE.iter().any(|n| url.contains(n))
}

fn redact_headers(h: &Value) -> Value {
    let mut out = Map::new();
    for (k, v) in h.as_object().into_iter().flatten() {
        let lower = k.to_lowercase();
        let value = match lower.as_str() {
            "cookie" | "set-cookie" => json!("<redacted>"),
            "authorization" => {
                let schemes: Vec<&str> = v
                    .as_str()
                    .unwrap_or("")
                    .split_whitespace()
                    .filter(|p| {
                        p.chars()
                            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                    })
                    .collect();
                json!(format!("{} <redacted>", schemes.join(" ")))
            }
            _ => v.clone(),
        };
        out.insert(k.clone(), value);
    }
    Value::Object(out)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…[truncated {} B]", &s[..end], s.len() - end)
}

fn textual(mime: &str) -> bool {
    mime.starts_with("text/")
        || mime.contains("json")
        || mime.contains("javascript")
        || mime.contains("protobuf")
        || mime.contains("xml")
        || mime.is_empty()
}

struct Recorder {
    out: BufWriter<File>,
    entries: HashMap<(String, String), Value>,
    pending: HashMap<u64, (String, String, &'static str)>,
    attached: HashSet<String>,
    page_session: Option<String>,
    written: usize,
}

impl Recorder {
    fn flush_entry(&mut self, key: &(String, String)) -> Result<()> {
        if let Some(e) = self.entries.remove(key) {
            writeln!(self.out, "{}", serde_json::to_string(&e)?)?;
            self.written += 1;
            self.out.flush()?;
        }
        Ok(())
    }

    fn handle(&mut self, b: &mut Browser, msg: Value) -> Result<()> {
        if let Some(id) = msg["id"].as_u64() {
            let Some((sid, rid, kind)) = self.pending.remove(&id) else {
                return Ok(());
            };
            let key = (sid, rid);
            if let Some(e) = self.entries.get_mut(&key) {
                let r = &msg["result"];
                match kind {
                    "body" => {
                        let body = r["body"].as_str().unwrap_or("");
                        let html = e["mime"].as_str().unwrap_or("").contains("html");
                        e["body_base64"] = r["base64Encoded"].clone();
                        e["body"] = json!(truncate(
                            body,
                            if html { MAX_HTML_BODY } else { MAX_TEXT_BODY }
                        ));
                    }
                    _ => {
                        e["post_data"] =
                            json!(truncate(r["postData"].as_str().unwrap_or(""), MAX_POST))
                    }
                }
            }
            if kind == "body" {
                self.flush_entry(&key)?;
            }
            return Ok(());
        }

        let method = msg["method"].as_str().unwrap_or("");
        let p = &msg["params"];
        let sid = msg["sessionId"].as_str().unwrap_or("").to_string();
        match method {
            "Target.targetCreated" | "Target.targetInfoChanged" => {
                let info = &p["targetInfo"];
                let tid = info["targetId"].as_str().unwrap_or("").to_string();
                if matches!(info["type"].as_str(), Some("page" | "iframe"))
                    && self.attached.insert(tid.clone())
                {
                    b.cdp.send(
                        "Target.attachToTarget",
                        json!({ "targetId": tid, "flatten": true }),
                        None,
                    )?;
                }
            }
            "Target.attachedToTarget" => {
                if let Some(s) = p["sessionId"].as_str() {
                    if p["targetInfo"]["type"] == "page" && self.page_session.is_none() {
                        self.page_session = Some(s.to_string());
                    }
                    b.cdp.send(
                        "Network.enable",
                        json!({ "maxPostDataSize": MAX_POST }),
                        Some(s),
                    )?;
                }
            }
            "Network.requestWillBeSent" => {
                let rid = p["requestId"].as_str().unwrap_or("").to_string();
                let key = (sid.clone(), rid.clone());
                if let Some(redirect) = p.get("redirectResponse") {
                    if let Some(e) = self.entries.get_mut(&key) {
                        e["status"] = redirect["status"].clone();
                        e["response_headers"] = redact_headers(&redirect["headers"]);
                    }
                    self.flush_entry(&key)?;
                }
                let req = &p["request"];
                let url = req["url"].as_str().unwrap_or("");
                let kind = p["type"].as_str().unwrap_or("Other");
                if !interesting(url, kind) {
                    return Ok(());
                }
                let mut e = json!({
                    "time": chrono::Local::now().to_rfc3339(),
                    "type": kind,
                    "method": req["method"],
                    "url": url,
                    "request_headers": redact_headers(&req["headers"]),
                });
                if let Some(pd) = req["postData"].as_str() {
                    e["post_data"] = json!(truncate(pd, MAX_POST));
                } else if req["hasPostData"].as_bool() == Some(true) {
                    let id = b.cdp.send(
                        "Network.getRequestPostData",
                        json!({ "requestId": rid }),
                        Some(&sid),
                    )?;
                    self.pending.insert(id, (sid.clone(), rid.clone(), "post"));
                }
                self.entries.insert(key, e);
            }
            "Network.responseReceived" => {
                let key = (sid, p["requestId"].as_str().unwrap_or("").to_string());
                if let Some(e) = self.entries.get_mut(&key) {
                    let r = &p["response"];
                    e["status"] = r["status"].clone();
                    e["mime"] = r["mimeType"].clone();
                    e["response_headers"] = redact_headers(&r["headers"]);
                }
            }
            "Network.loadingFinished" => {
                let rid = p["requestId"].as_str().unwrap_or("").to_string();
                let key = (sid.clone(), rid.clone());
                let Some(e) = self.entries.get_mut(&key) else {
                    return Ok(());
                };
                e["size"] = p["encodedDataLength"].clone();
                if textual(e["mime"].as_str().unwrap_or("")) {
                    let id = b.cdp.send(
                        "Network.getResponseBody",
                        json!({ "requestId": rid }),
                        Some(&sid),
                    )?;
                    self.pending.insert(id, (sid, rid, "body"));
                } else {
                    self.flush_entry(&key)?;
                }
            }
            "Network.loadingFailed" => {
                let key = (sid, p["requestId"].as_str().unwrap_or("").to_string());
                if let Some(e) = self.entries.get_mut(&key) {
                    e["error"] = p["errorText"].clone();
                }
                self.flush_entry(&key)?;
            }
            _ => {}
        }
        Ok(())
    }
}

pub fn capture(ctx: &Ctx) -> Result<()> {
    let query = ctx
        .account
        .clone()
        .or_else(|| ctx.cfg.active_account.clone());
    let mut sess = match query {
        Some(q) => session::find(&q)?,
        None => match session::list()?.into_iter().next() {
            Some(s) => s,
            None => bail!("Log in first: class auth login"),
        },
    };
    let profile = session::profile_dir(&sess.email);
    if !profile.exists() {
        bail!(
            "No browser profile for {}. Log in again: class auth login",
            sess.email
        );
    }

    let dir = config::cache_dir().join("captures");
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!(
        "capture-{}.jsonl",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    let mut rec = Recorder {
        out: BufWriter::new(File::create(&path)?),
        entries: HashMap::new(),
        pending: HashMap::new(),
        attached: HashSet::new(),
        page_session: None,
        written: 0,
    };

    let mut b = Browser::launch(&sess.browser_binary()?, &profile, "about:blank", false)?;
    b.cdp.call(
        "Target.setDiscoverTargets",
        json!({ "discover": true }),
        None,
    )?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while rec.page_session.is_none() && std::time::Instant::now() < deadline {
        if let Some(msg) = b.cdp.next(Duration::from_millis(100))? {
            rec.handle(&mut b, msg)?;
        }
    }
    let Some(page) = rec.page_session.clone() else {
        b.close();
        bail!("Could not attach to the browser tab");
    };
    b.cdp.send(
        "Page.navigate",
        json!({ "url": "https://classroom.google.com/" }),
        Some(&page),
    )?;

    println!(
        "{} Recording Classroom network traffic ({}).",
        style("●").red(),
        sess.email
    );
    println!("In the browser window, do the following:");
    println!("  1. Open the assignment list (To-do)");
    println!("  2. Open an assignment with a teacher attachment and download the attachment");
    println!("  3. 'Add or create' → 'File' → upload any file (e.g. a zip)");
    println!("  4. Click 'Turn in'");
    println!("  5. Optionally: 'Unsubmit'");
    println!(
        "\nThen come back here and press {} (or close the browser).",
        style("Enter").bold()
    );

    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().lock().read_line(&mut line);
        flag.store(true, Ordering::SeqCst);
    });

    let mut browser_open = true;
    while !stop.load(Ordering::SeqCst) {
        match b.cdp.next(Duration::from_millis(200)) {
            Ok(Some(msg)) => rec.handle(&mut b, msg)?,
            Ok(None) => {}
            Err(_) => {
                browser_open = false;
                break;
            }
        }
    }

    if browser_open {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !rec.pending.is_empty() && std::time::Instant::now() < deadline {
            match b.cdp.next(Duration::from_millis(200)) {
                Ok(Some(msg)) => rec.handle(&mut b, msg)?,
                Ok(None) => {}
                Err(_) => break,
            }
        }
        if let Ok(cookies) = session::read_cookies(&mut b.cdp)
            && !cookies.is_empty()
        {
            sess.cookies = cookies;
            sess.saved_at = chrono::Local::now().to_rfc3339();
            session::save(&sess)?;
        }
        b.close();
    }
    let keys: Vec<_> = rec.entries.keys().cloned().collect();
    for k in keys {
        rec.flush_entry(&k)?;
    }

    println!(
        "\n{} Saved {} requests to:",
        style("✓").green(),
        rec.written
    );
    println!("  {}", path.display());
    println!(
        "{}",
        style("Cookies and Authorization headers are redacted, but the file contains data from your Classroom. Do not share it publicly.").dim()
    );
    Ok(())
}

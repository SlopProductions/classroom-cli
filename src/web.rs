use crate::session::{self, CHROME_UA, Session};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use indicatif::ProgressBar;
use reqwest::blocking::{Body, Client, RequestBuilder, Response};
use reqwest::cookie::Jar;
use reqwest::header::{CONTENT_TYPE, LOCATION, ORIGIN, REFERER, USER_AGENT};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

const CLASSROOM: &str = "https://classroom.google.com";
const DRIVE: &str = "https://drive.google.com";
const DRIVE_API_ROOT: &str = "https://clients6.google.com/drive/v2internal";
const DRIVE_API: &str = "https://clients6.google.com/drive/v2internal/files";
const DRIVE_UPLOAD: &str = "https://clients6.google.com/upload/drive/v2internal/files";
const DRIVE_EXT: &str = r#"[[907,null,null,null,0,null,null,"CLASSROOM",null,null,null,[2]]]"#;

#[derive(Debug)]
struct LoggedOut;
impl std::fmt::Display for LoggedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Google session expired")
    }
}
impl std::error::Error for LoggedOut {}

struct Boot {
    api_keys: Vec<String>,
    authuser: String,
    user_id: String,
    at: String,
    fsid: String,
    bl: String,
}

pub struct Web {
    api_keys: Vec<String>,
    drive_key: OnceLock<String>,
    http: Client,
    session: Session,
    pub email: String,
    pub authuser: String,
    pub user_id: String,
    at: String,
    fsid: String,
    bl: String,
}

fn build_client(session: &Session) -> Result<Client> {
    let jar = Jar::default();
    let now = chrono::Utc::now().timestamp() as f64;
    for c in &session.cookies {
        if c.expires > 0.0 && c.expires < now {
            continue;
        }
        let host = c.domain.trim_start_matches('.');
        let mut s = format!("{}={}; Path={}", c.name, c.value, c.path);
        if c.domain.starts_with('.') {
            s.push_str(&format!("; Domain={host}"));
        }
        if c.secure {
            s.push_str("; Secure");
        }
        if let Ok(url) = format!("https://{host}{}", c.path).parse() {
            jar.add_cookie_str(&s, &url);
        }
    }
    Ok(Client::builder()
        .cookie_provider(Arc::new(jar))
        .timeout(Duration::from_secs(900))
        .build()?)
}

fn wiz<'a>(html: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\":\"");
    let start = html.find(&pat)? + pat.len();
    let end = html[start..].find('"')? + start;
    Some(&html[start..end])
}

fn user_id(html: &str) -> Option<String> {
    let pat = "\"x3UoP\":\"%.@.[\\\"";
    let start = html.find(pat)? + pat.len();
    let id: String = html[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    (!id.is_empty()).then_some(id)
}

fn api_keys(html: &str) -> Vec<String> {
    let mut keys: Vec<String> = vec![];
    let mut rest = html;
    while let Some(i) = rest.find("AIza") {
        let key: String = rest[i..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        if key.len() == 39 && !keys.contains(&key) {
            keys.push(key);
        }
        rest = &rest[i + 4..];
    }
    keys
}

fn bootstrap(http: &Client, email: &str) -> Result<Boot> {
    let resp = http
        .get(CLASSROOM)
        .query(&[("authuser", email)])
        .header(USER_AGENT, CHROME_UA)
        .send()?;
    let url = resp.url().clone();
    let html = resp.text()?;
    if url.host_str() != Some("classroom.google.com") {
        return Err(anyhow!(LoggedOut));
    }
    let x3: String = html
        .find("\"x3UoP\"")
        .map(|i| html[i..].chars().take(400).collect())
        .unwrap_or_default();
    if !x3.to_lowercase().contains(&email.to_lowercase()) {
        return Err(anyhow!(LoggedOut));
    }
    let authuser = url
        .path_segments()
        .and_then(|mut s| {
            if s.next() == Some("u") {
                s.next().map(String::from)
            } else {
                None
            }
        })
        .unwrap_or_else(|| "0".into());
    Ok(Boot {
        api_keys: api_keys(&html),
        authuser,
        user_id: user_id(&html).context("Student ID not found on the Classroom page")?,
        at: wiz(&html, "SNlM0e").context("Missing SNlM0e token")?.into(),
        fsid: wiz(&html, "FdrFJe").unwrap_or("").into(),
        bl: wiz(&html, "cfb2h").unwrap_or("").into(),
    })
}

fn check(resp: Response) -> Result<Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let url = resp.url().to_string();
    let body = resp.text().unwrap_or_default();
    let snippet: String = body.chars().take(300).collect();
    bail!("HTTP {status} from {url}: {snippet}")
}

fn parse_batch(text: &str, rpc: &str) -> Result<Value> {
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with('[') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        for entry in v.as_array().into_iter().flatten() {
            if entry[0] == "wrb.fr" && entry[1] == rpc {
                return match entry[2].as_str() {
                    Some(payload) => Ok(serde_json::from_str(payload)?),
                    None => bail!("Classroom rejected request {rpc} (code {})", entry[5]),
                };
            }
        }
    }
    bail!("No Classroom response for {rpc}")
}

impl Web {
    pub fn connect(mut session: Session, status: &dyn Fn(&str)) -> Result<Self> {
        let http = build_client(&session)?;
        let boot = match bootstrap(&http, &session.email) {
            Ok(b) => Some(b),
            Err(e) if e.downcast_ref::<LoggedOut>().is_some() => None,
            Err(e) => return Err(e),
        };
        let (http, boot) = match boot {
            Some(b) => (http, b),
            None => {
                status("Refreshing Google session");
                session::refresh(&mut session)?;
                let http = build_client(&session)?;
                let b = bootstrap(&http, &session.email).map_err(|_| {
                    anyhow!(
                        "Session for {} expired. Log in again: class auth login",
                        session.email
                    )
                })?;
                (http, b)
            }
        };
        Ok(Self {
            http,
            email: session.email.clone(),
            session,
            authuser: boot.authuser,
            user_id: boot.user_id,
            at: boot.at,
            fsid: boot.fsid,
            bl: boot.bl,
            api_keys: boot.api_keys,
            drive_key: OnceLock::new(),
        })
    }

    pub fn rpc(&self, rpc: &str, args: &Value, source_path: &str) -> Result<Value> {
        let n = &self.authuser;
        let freq = json!([[[rpc, args.to_string(), null, "generic"]]]).to_string();
        let reqid = (rand::random::<u32>() % 900_000 + 100_000).to_string();
        let source = format!("/u/{n}{source_path}");
        let resp = self
            .http
            .post(format!("{CLASSROOM}/u/{n}/_/ClassroomUi/data/batchexecute"))
            .query(&[
                ("rpcids", rpc),
                ("source-path", &source),
                ("f.sid", &self.fsid),
                ("bl", &self.bl),
                ("hl", "en"),
                ("_reqid", &reqid),
                ("rt", "c"),
            ])
            .header(USER_AGENT, CHROME_UA)
            .header(ORIGIN, CLASSROOM)
            .header(REFERER, format!("{CLASSROOM}/"))
            .header("x-same-domain", "1")
            .form(&[("f.req", freq.as_str()), ("at", &self.at)])
            .send()
            .with_context(|| format!("Connection to Classroom failed ({rpc})"))?;
        parse_batch(&check(resp)?.text()?, rpc)
    }

    fn sapisid_hash(&self, origin: &str) -> String {
        let ts = chrono::Utc::now().timestamp();
        [
            ("SAPISIDHASH", "SAPISID"),
            ("SAPISID1PHASH", "__Secure-1PAPISID"),
            ("SAPISID3PHASH", "__Secure-3PAPISID"),
        ]
        .iter()
        .filter_map(|(scheme, cookie)| {
            let value = self.session.cookie(cookie)?;
            let hash = Sha1::digest(format!("{ts} {value} {origin}").as_bytes());
            Some(format!("{scheme} {ts}_{hash:x}"))
        })
        .collect::<Vec<_>>()
        .join(" ")
    }

    fn drive(&self, rb: RequestBuilder) -> RequestBuilder {
        rb.header(USER_AGENT, CHROME_UA)
            .header(ORIGIN, DRIVE)
            .header(REFERER, format!("{DRIVE}/"))
            .header("authorization", self.sapisid_hash(DRIVE))
            .header("x-goog-authuser", &self.authuser)
    }

    fn drive_key(&self) -> Result<&str> {
        if let Some(k) = self.drive_key.get() {
            return Ok(k);
        }
        for key in &self.api_keys {
            let probe = self
                .drive(
                    self.http
                        .get(format!("{DRIVE_API_ROOT}/changes/startPageToken")),
                )
                .query(&[("key", key.as_str())])
                .send();
            if probe.is_ok_and(|r| r.status().is_success()) {
                return Ok(self.drive_key.get_or_init(|| key.clone()));
            }
        }
        bail!("No usable Google Drive API key found on the Classroom page")
    }

    pub fn drive_meta(&self, id: &str) -> Result<Value> {
        let rb = self.http.get(format!("{DRIVE_API}/{id}")).query(&[
            ("key", self.drive_key()?),
            (
                "fields",
                "id,title,mimeType,fileSize,exportLinks,alternateLink",
            ),
            ("supportsTeamDrives", "true"),
        ]);
        Ok(check(self.drive(rb).send()?)?.json()?)
    }

    pub fn drive_download_url(&self, id: &str) -> String {
        format!(
            "{DRIVE}/uc?export=download&confirm=t&id={id}&authuser={}",
            self.authuser
        )
    }

    pub fn download(&self, url: &str) -> Result<(Vec<u8>, String)> {
        let resp = check(self.http.get(url).header(USER_AGENT, CHROME_UA).send()?)?;
        let ctype = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        Ok((resp.bytes()?.to_vec(), ctype))
    }

    pub fn upload(
        &self,
        path: &Path,
        name: &str,
        progress: &ProgressBar,
    ) -> Result<(String, String)> {
        let mime = mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string();
        let len = std::fs::metadata(path)?.len();
        let query = [
            ("uploadType", "resumable"),
            ("supportsTeamDrives", "true"),
            ("OriginatorHint", "CLASSROOM"),
            ("reason", "907"),
            ("syncType", "0"),
            ("openDrive", "false"),
            ("errorRecovery", "false"),
            ("pinned", "false"),
            ("retryCount", "0"),
            ("fields", "id,title,mimeType,alternateLink"),
            ("key", self.drive_key()?),
        ];
        let init = self
            .drive(self.http.post(DRIVE_UPLOAD).query(&query))
            .header("X-Upload-Content-Type", &mime)
            .header("X-Upload-Content-Length", len)
            .header("x-goog-ext-525001598-jspb", STANDARD.encode(DRIVE_EXT))
            .json(&json!({ "title": name, "mimeType": mime }))
            .send()?;
        let init = check(init)?;
        let session_url = init
            .headers()
            .get(LOCATION)
            .context("Drive did not return an upload URL")?
            .to_str()?
            .to_string();
        let mut put = self
            .drive(self.http.put(session_url))
            .header(CONTENT_TYPE, &mime);
        if len > 0 {
            put = put.header("content-range", format!("bytes 0-{}/{len}", len - 1));
        }
        let reader = progress.wrap_read(std::fs::File::open(path)?);
        let resp = put.body(Body::sized(reader, len)).send()?;
        let file: Value = check(resp)?.json()?;
        let id = file["id"]
            .as_str()
            .context("Drive did not return a file ID")?
            .to_string();
        Ok((id, mime))
    }
}

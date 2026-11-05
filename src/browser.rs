use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::fs;
use std::io::ErrorKind;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};
use tungstenite::{Message, WebSocket};

pub struct Kind {
    pub name: &'static str,
    binaries: &'static [&'static str],
    user_data: &'static str,
}

pub const KINDS: &[Kind] = &[
    Kind {
        name: "chrome",
        binaries: &["google-chrome-stable", "google-chrome"],
        user_data: "google-chrome",
    },
    Kind {
        name: "chromium",
        binaries: &["chromium", "chromium-browser"],
        user_data: "chromium",
    },
    Kind {
        name: "helium",
        binaries: &["helium-browser", "helium"],
        user_data: "net.imput.helium",
    },
    Kind {
        name: "brave",
        binaries: &["brave", "brave-browser"],
        user_data: "BraveSoftware/Brave-Browser",
    },
    Kind {
        name: "edge",
        binaries: &["microsoft-edge-stable", "microsoft-edge"],
        user_data: "microsoft-edge",
    },
];

pub fn kind(name: &str) -> Result<&'static Kind> {
    let n = name.to_lowercase();
    KINDS.iter().find(|k| k.name == n).with_context(|| {
        let names: Vec<_> = KINDS.iter().map(|k| k.name).collect();
        format!("Unknown browser '{name}'. Available: {}", names.join(", "))
    })
}

fn which(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

impl Kind {
    pub fn binary(&self) -> Result<PathBuf> {
        self.binaries
            .iter()
            .find_map(|b| which(b))
            .with_context(|| format!("Browser {} not found in PATH", self.name))
    }

    pub fn user_data_dir(&self) -> PathBuf {
        dirs::config_dir().unwrap_or_default().join(self.user_data)
    }
}

pub fn resolve(preferred: Option<&str>) -> Result<PathBuf> {
    if let Ok(p) = std::env::var("CLASS_CHROME") {
        return Ok(p.into());
    }
    if let Some(p) = preferred {
        if p.contains('/') {
            return Ok(p.into());
        }
        return kind(p)?.binary();
    }
    if let Some(p) = KINDS.iter().find_map(|k| k.binary().ok()) {
        return Ok(p);
    }
    let mac = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
    if Path::new(mac).is_file() {
        return Ok(mac.into());
    }
    bail!(
        "No Chromium-based browser found. Set one with: class config set browser <chrome|helium|brave|...>"
    )
}

pub struct Browser {
    child: Child,
    pub cdp: Cdp,
}

impl Browser {
    pub fn launch(binary: &Path, profile: &Path, url: &str, headless: bool) -> Result<Self> {
        fs::create_dir_all(profile)?;
        let port_file = profile.join("DevToolsActivePort");
        let _ = fs::remove_file(&port_file);
        let mut cmd = Command::new(binary);
        if headless {
            cmd.arg("--headless=new");
        }
        let child = cmd
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg("--remote-debugging-port=0")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("Could not start {}", binary.display()))?;

        let deadline = Instant::now() + Duration::from_secs(20);
        let ws_url = loop {
            if let Ok(s) = fs::read_to_string(&port_file) {
                let mut lines = s.lines();
                if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                    break format!("ws://127.0.0.1:{port}{path}");
                }
            }
            if Instant::now() > deadline {
                bail!(
                    "The browser did not start in debugging mode.\nIf a window with the class profile is already open, close it and try again."
                );
            }
            sleep(Duration::from_millis(150));
        };
        Ok(Self {
            child,
            cdp: Cdp::connect(&ws_url)?,
        })
    }

    pub fn close(mut self) {
        let _ = self.cdp.send("Browser.close", json!({}), None);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct Cdp {
    ws: WebSocket<TcpStream>,
    next_id: u64,
    queue: VecDeque<Value>,
}

impl Cdp {
    fn connect(url: &str) -> Result<Self> {
        let u = url::Url::parse(url)?;
        let addr = format!(
            "{}:{}",
            u.host_str().unwrap_or("127.0.0.1"),
            u.port().unwrap_or(9222)
        );
        let stream = TcpStream::connect(addr)?;
        let (ws, _) =
            tungstenite::client(url, stream).map_err(|e| anyhow!("Chrome connection: {e}"))?;
        Ok(Self {
            ws,
            next_id: 1,
            queue: VecDeque::new(),
        })
    }

    pub fn send(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        self.ws
            .send(Message::text(msg.to_string()))
            .map_err(closed)?;
        Ok(id)
    }

    pub fn call(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        let id = self.send(method, params, session)?;
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if let Some(msg) = self.read(Duration::from_millis(500))? {
                if msg["id"].as_u64() == Some(id) {
                    if let Some(err) = msg.get("error") {
                        bail!("CDP {method}: {err}");
                    }
                    return Ok(msg["result"].clone());
                }
                self.queue.push_back(msg);
            }
        }
        bail!("Chrome did not respond to {method}")
    }

    pub fn next(&mut self, timeout: Duration) -> Result<Option<Value>> {
        if let Some(m) = self.queue.pop_front() {
            return Ok(Some(m));
        }
        self.read(timeout)
    }

    fn read(&mut self, timeout: Duration) -> Result<Option<Value>> {
        self.ws.get_mut().set_read_timeout(Some(timeout))?;
        match self.ws.read() {
            Ok(Message::Text(t)) => Ok(Some(serde_json::from_str(&t)?)),
            Ok(Message::Binary(b)) => Ok(Some(serde_json::from_slice(&b)?)),
            Ok(_) => Ok(None),
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                Ok(None)
            }
            Err(e) => Err(closed(e)),
        }
    }
}

fn closed(e: tungstenite::Error) -> anyhow::Error {
    match e {
        tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed => {
            anyhow!(BrowserClosed)
        }
        tungstenite::Error::Io(ref io)
            if matches!(
                io.kind(),
                ErrorKind::ConnectionReset | ErrorKind::BrokenPipe | ErrorKind::UnexpectedEof
            ) =>
        {
            anyhow!(BrowserClosed)
        }
        e => anyhow!("Chrome connection error: {e}"),
    }
}

#[derive(Debug)]
pub struct BrowserClosed;
impl std::fmt::Display for BrowserClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Chrome window was closed")
    }
}
impl std::error::Error for BrowserClosed {}

pub fn is_closed(e: &anyhow::Error) -> bool {
    e.downcast_ref::<BrowserClosed>().is_some()
}

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
    pub label: &'static str,
    linux_bins: &'static [&'static str],
    mac_apps: &'static [&'static str],
    mac_bundle_id: &'static str,
    linux_data: &'static str,
    mac_data: &'static str,
}

pub const KINDS: &[Kind] = &[
    Kind {
        name: "chrome",
        label: "Google Chrome",
        linux_bins: &["google-chrome-stable", "google-chrome"],
        mac_apps: &["Google Chrome.app"],
        mac_bundle_id: "com.google.Chrome",
        linux_data: "google-chrome",
        mac_data: "Google/Chrome",
    },
    Kind {
        name: "helium",
        label: "Helium",
        linux_bins: &["helium-browser", "helium"],
        mac_apps: &["Helium.app"],
        mac_bundle_id: "net.imput.helium",
        linux_data: "net.imput.helium",
        mac_data: "net.imput.helium",
    },
    Kind {
        name: "brave",
        label: "Brave",
        linux_bins: &["brave", "brave-browser"],
        mac_apps: &["Brave Browser.app"],
        mac_bundle_id: "com.brave.Browser",
        linux_data: "BraveSoftware/Brave-Browser",
        mac_data: "BraveSoftware/Brave-Browser",
    },
    Kind {
        name: "arc",
        label: "Arc",
        linux_bins: &[],
        mac_apps: &["Arc.app"],
        mac_bundle_id: "company.thebrowser.Browser",
        linux_data: "",
        mac_data: "Arc/User Data",
    },
    Kind {
        name: "edge",
        label: "Microsoft Edge",
        linux_bins: &["microsoft-edge-stable", "microsoft-edge"],
        mac_apps: &["Microsoft Edge.app"],
        mac_bundle_id: "com.microsoft.edgemac",
        linux_data: "microsoft-edge",
        mac_data: "Microsoft Edge",
    },
    Kind {
        name: "chromium",
        label: "Chromium",
        linux_bins: &["chromium", "chromium-browser"],
        mac_apps: &["Chromium.app"],
        mac_bundle_id: "org.chromium.Chromium",
        linux_data: "chromium",
        mac_data: "Chromium",
    },
    Kind {
        name: "vivaldi",
        label: "Vivaldi",
        linux_bins: &["vivaldi-stable", "vivaldi"],
        mac_apps: &["Vivaldi.app"],
        mac_bundle_id: "com.vivaldi.Vivaldi",
        linux_data: "vivaldi",
        mac_data: "Vivaldi",
    },
    Kind {
        name: "opera",
        label: "Opera",
        linux_bins: &["opera"],
        mac_apps: &["Opera.app"],
        mac_bundle_id: "com.operasoftware.Opera",
        linux_data: "opera",
        mac_data: "com.operasoftware.Opera",
    },
];

const IS_MAC: bool = cfg!(target_os = "macos");

pub fn names() -> String {
    KINDS.iter().map(|k| k.name).collect::<Vec<_>>().join(", ")
}

pub fn kind(name: &str) -> Result<&'static Kind> {
    let n = name.to_lowercase();
    KINDS
        .iter()
        .find(|k| k.name == n)
        .with_context(|| format!("Unknown browser '{name}'. Known: {}", names()))
}

fn which(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

fn mac_app_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/Applications")];
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join("Applications"));
    }
    dirs
}

fn mac_executable(app: &Path) -> Option<PathBuf> {
    let macos = app.join("Contents/MacOS");
    let from_plist = Command::new("plutil")
        .args(["-extract", "CFBundleExecutable", "raw", "-o", "-"])
        .arg(app.join("Contents/Info.plist"))
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|exe| macos.join(exe))
        .filter(|p| p.is_file());
    if from_plist.is_some() {
        return from_plist;
    }
    let by_name = app
        .file_stem()
        .map(|stem| macos.join(stem))
        .filter(|p| p.is_file());
    if by_name.is_some() {
        return by_name;
    }
    fs::read_dir(&macos)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.is_file())
}

fn mac_find_app(kind: &Kind) -> Option<PathBuf> {
    for dir in mac_app_dirs() {
        for app in kind.mac_apps {
            let path = dir.join(app);
            if path.is_dir() {
                return Some(path);
            }
        }
    }
    let out = Command::new("mdfind")
        .arg(format!(
            "kMDItemCFBundleIdentifier == '{}'",
            kind.mac_bundle_id
        ))
        .stderr(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(PathBuf::from)
        .find(|p| p.extension().is_some_and(|e| e == "app") && p.is_dir())
}

impl Kind {
    pub fn binary(&self) -> Result<PathBuf> {
        if IS_MAC && let Some(exe) = mac_find_app(self).and_then(|app| mac_executable(&app)) {
            return Ok(exe);
        }
        self.linux_bins
            .iter()
            .find_map(|b| which(b))
            .with_context(|| {
                format!(
                    "{} is not installed or was not found. Point to it with --browser <path>",
                    self.label
                )
            })
    }

    pub fn user_data_dir(&self) -> PathBuf {
        let rel = if IS_MAC {
            self.mac_data
        } else {
            self.linux_data
        };
        dirs::config_dir().unwrap_or_default().join(rel)
    }

    pub fn has_profile(&self) -> bool {
        let rel = if IS_MAC {
            self.mac_data
        } else {
            self.linux_data
        };
        !rel.is_empty() && self.user_data_dir().join("Local State").is_file()
    }
}

pub fn installed() -> Vec<(&'static Kind, PathBuf)> {
    KINDS
        .iter()
        .filter_map(|k| k.binary().ok().map(|b| (k, b)))
        .collect()
}

pub fn executable(path: &Path) -> Result<PathBuf> {
    if path.extension().is_some_and(|e| e == "app") {
        return mac_executable(path)
            .with_context(|| format!("No executable found inside {}", path.display()));
    }
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    bail!("Browser not found at {}", path.display())
}

pub fn resolve(preferred: Option<&str>) -> Result<PathBuf> {
    if let Ok(p) = std::env::var("CLASS_CHROME") {
        return executable(Path::new(&p));
    }
    if let Some(p) = preferred {
        if p.contains('/') {
            return executable(Path::new(p));
        }
        return kind(p)?.binary();
    }
    installed()
        .into_iter()
        .next()
        .map(|(_, bin)| bin)
        .with_context(|| {
            format!(
                "No Chromium-based browser found ({}). Install one or pass --browser <path>",
                names()
            )
        })
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

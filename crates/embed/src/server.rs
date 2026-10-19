//! Running llama.cpp's `llama-server` for embeddings, as Jan learned to:
//! on a free loopback port, its key in the environment (never in argv,
//! which `ps` shows), from its own folder (it loads its GPU modules from
//! there), with a client that ignores proxies, its output read and kept
//! for errors, and stopped with the app. A server left by a crash is
//! stopped at the next start.

use std::{
    collections::VecDeque,
    io::{BufRead as _, BufReader},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};

/// The marker of our servers on their command line.
pub const ALIAS: &str = "glyphst-embed";
const LOG_LINES: usize = 40;
const START_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub exe: PathBuf,
    pub model: PathBuf,
    /// Folders to add to the library path: on Linux, the Vulkan loader the
    /// app itself loaded (NixOS has none where llama.cpp's build looks).
    pub library_dirs: Vec<PathBuf>,
    /// Where the running server's id is kept, to stop it after a crash.
    pub pid_file: Option<PathBuf>,
}

/// Why the server could not serve.
#[derive(Debug)]
pub enum ServerError {
    /// It exited while starting; the end of what it said.
    Exited(String),
    /// It did not answer in time.
    Timeout(String),
    Other(anyhow::Error),
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exited(log) => write!(formatter, "the engine stopped: {log}"),
            Self::Timeout(log) => write!(formatter, "the engine did not start in time: {log}"),
            Self::Other(error) => write!(formatter, "{error:#}"),
        }
    }
}

impl std::error::Error for ServerError {}

/// A running server, stopped when dropped.
pub struct Server {
    child: Mutex<Child>,
    client: Client,
    log: Arc<Mutex<VecDeque<String>>>,
    pid_file: Option<PathBuf>,
    /// The GPUs it found; empty: the CPU.
    pub devices: Vec<String>,
}

impl Server {
    pub fn start(config: &ServerConfig) -> Result<Self, ServerError> {
        if let Some(pid_file) = &config.pid_file {
            stop_leftover(pid_file);
        }
        let devices = list_devices(config);
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .map(|address| address.port())
            .map_err(|error| ServerError::Other(error.into()))?;
        let key = (0..4)
            .map(|_| format!("{:016x}", rand::random::<u64>()))
            .collect::<String>();
        let mut command = base_command(config);
        command
            .arg("-m")
            .arg(&config.model)
            .args(["--embeddings", "--pooling", "mean"])
            // The model's 8K context per passage, not the server's default.
            .args(["-c", "8192", "-ub", "2048", "-b", "2048"])
            .args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .args(["--alias", ALIAS, "--no-ui"])
            .env("LLAMA_API_KEY", &key)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start {}", config.exe.display()))
            .map_err(ServerError::Other)?;
        if let Some(pid_file) = &config.pid_file {
            let _ = std::fs::write(pid_file, child.id().to_string());
        }
        let log = Arc::new(Mutex::new(VecDeque::new()));
        for stream in [
            child
                .stdout
                .take()
                .map(|out| Box::new(out) as Box<dyn std::io::Read + Send>),
            child
                .stderr
                .take()
                .map(|err| Box::new(err) as Box<dyn std::io::Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let log = log.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    if let Ok(mut log) = log.lock() {
                        if log.len() >= LOG_LINES {
                            log.pop_front();
                        }
                        log.push_back(line);
                    }
                }
            });
        }
        let server = Self {
            child: Mutex::new(child),
            client: Client::new(format!("http://127.0.0.1:{port}"), key),
            log,
            pid_file: config.pid_file.clone(),
            devices,
        };
        server.wait_ready()?;
        Ok(server)
    }

    fn wait_ready(&self) -> Result<(), ServerError> {
        let started = Instant::now();
        loop {
            if let Ok(mut child) = self.child.lock()
                && let Ok(Some(_)) = child.try_wait()
            {
                // Its last words arrive just after it exits.
                std::thread::sleep(Duration::from_millis(100));
                return Err(ServerError::Exited(self.log_tail()));
            }
            if self.client.healthy() {
                return Ok(());
            }
            if started.elapsed() > START_TIMEOUT {
                return Err(ServerError::Timeout(self.log_tail()));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// Whether it still runs.
    pub fn is_alive(&self) -> bool {
        self.child
            .lock()
            .is_ok_and(|mut child| matches!(child.try_wait(), Ok(None)))
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// What it said last, an error's likely cause last.
    pub fn log_tail(&self) -> String {
        let log = self.log.lock().map(|log| log.clone()).unwrap_or_default();
        let important = log
            .iter()
            .rev()
            .find(|line| {
                let lower = line.to_lowercase();
                lower.contains("error")
                    || lower.contains("failed")
                    || lower.contains("out of memory")
            })
            .or_else(|| log.back());
        important.cloned().unwrap_or_default()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(pid_file) = &self.pid_file {
            let _ = std::fs::remove_file(pid_file);
        }
    }
}

fn base_command(config: &ServerConfig) -> Command {
    let mut command = Command::new(&config.exe);
    // It loads its CPU and GPU modules from its folder; not from wherever
    // the app runs.
    if let Some(dir) = config.exe.parent() {
        command.current_dir(dir);
    }
    if !config.library_dirs.is_empty() {
        let mut dirs = config.library_dirs.clone();
        if let Some(existing) = std::env::var_os("LD_LIBRARY_PATH") {
            dirs.extend(std::env::split_paths(&existing));
        }
        if let Ok(joined) = std::env::join_paths(dirs) {
            command.env("LD_LIBRARY_PATH", joined);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // No console window flashing up.
        command.creation_flags(0x0800_0000);
    }
    command
}

/// The GPUs the engine can use (`--list-devices`); empty: only the CPU.
pub fn list_devices(config: &ServerConfig) -> Vec<String> {
    let Ok(output) = base_command(config).arg("--list-devices").output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .chain(String::from_utf8_lossy(&output.stderr).lines())
        .skip_while(|line| !line.contains("Available devices"))
        .skip(1)
        .map(str::trim)
        .filter(|line| !line.is_empty() && *line != "(none)")
        .map(str::to_string)
        .collect()
}

/// Stops a server an earlier run left (it crashed), if that pid is still
/// one of ours.
fn stop_leftover(pid_file: &Path) {
    let Some(pid) = std::fs::read_to_string(pid_file)
        .ok()
        .and_then(|pid| pid.trim().parse::<u32>().ok())
    else {
        return;
    };
    let _ = std::fs::remove_file(pid_file);
    #[cfg(unix)]
    {
        let ours = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "command="])
            .output()
            .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains(ALIAS));
        if ours {
            let _ = Command::new("kill").arg(pid.to_string()).status();
        }
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .status();
    }
}

/// Where the Vulkan loader this process uses lives, for the server.
pub fn vulkan_dirs() -> Vec<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else {
            return Vec::new();
        };
        let mut dirs = maps
            .lines()
            .filter_map(|line| line.split_whitespace().nth(5))
            .filter(|path| {
                Path::new(path)
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("libvulkan.so"))
            })
            .filter_map(|path| Path::new(path).parent().map(Path::to_path_buf))
            .collect::<Vec<_>>();
        dirs.dedup();
        dirs
    }
    #[cfg(not(target_os = "linux"))]
    {
        Vec::new()
    }
}

/// The server's HTTP side.
pub struct Client {
    base: String,
    key: String,
    http: reqwest::blocking::Client,
}

impl Client {
    pub fn new(base: String, key: String) -> Self {
        let http = reqwest::blocking::Client::builder()
            // A VPN or proxy must not catch loopback requests.
            .no_proxy()
            .timeout(Duration::from_secs(300))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self { base, key, http }
    }

    fn post(&self, path: &str, body: &Value) -> Result<Value> {
        let response = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.key)
            .json(body)
            .send()?;
        let status = response.status();
        let value: Value = response.json().unwrap_or(Value::Null);
        if !status.is_success() {
            bail!(
                "the engine answered {status}: {}",
                value["error"]["message"].as_str().unwrap_or_default()
            );
        }
        Ok(value)
    }

    pub fn healthy(&self) -> bool {
        self.http
            .get(format!("{}/health", self.base))
            .bearer_auth(&self.key)
            .timeout(Duration::from_secs(2))
            .send()
            .is_ok_and(|response| response.status().is_success())
    }

    /// One vector per input, in order; refuses NaN, infinity or empty ones.
    pub fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let reply = self.post(
            "/v1/embeddings",
            &json!({"input": inputs, "encoding_format": "float"}),
        )?;
        let mut data = reply["data"]
            .as_array()
            .ok_or_else(|| anyhow!("the engine gave no vectors"))?
            .iter()
            .map(|item| {
                let index = item["index"].as_u64().unwrap_or(0) as usize;
                let vector = item["embedding"]
                    .as_array()
                    .map(|values| {
                        values
                            .iter()
                            .map(|value| value.as_f64().unwrap_or(f64::NAN) as f32)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                (index, vector)
            })
            .collect::<Vec<_>>();
        data.sort_by_key(|(index, _)| *index);
        if data.len() != inputs.len() {
            bail!(
                "the engine gave {} vectors for {} inputs",
                data.len(),
                inputs.len()
            );
        }
        for (_, vector) in &data {
            if vector.is_empty()
                || vector.iter().any(|value| !value.is_finite())
                || vector.iter().all(|value| *value == 0.0)
            {
                bail!("the engine gave a broken vector (NaN or empty)");
            }
        }
        Ok(data.into_iter().map(|(_, vector)| vector).collect())
    }

    /// How many tokens `text` is.
    pub fn count_tokens(&self, text: &str) -> Result<usize> {
        let reply = self.post("/tokenize", &json!({"content": text}))?;
        Ok(reply["tokens"].as_array().map_or(0, Vec::len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(answer: Value, status: u16) -> String {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let address = format!("http://{}", server.server_addr().to_ip().unwrap());
        std::thread::spawn(move || {
            for request in server.incoming_requests() {
                let authorized = request.headers().iter().any(|header| {
                    header.field.equiv("Authorization") && header.value.as_str() == "Bearer key"
                });
                let (status, body) = if authorized {
                    (status, answer.to_string())
                } else {
                    (401, "{}".to_string())
                };
                let _ = request
                    .respond(tiny_http::Response::from_string(body).with_status_code(status));
            }
        });
        address
    }

    #[test]
    fn vectors_come_back_in_order_and_broken_ones_are_refused() {
        let base = fake(
            json!({"data": [
                {"index": 1, "embedding": [0.0, 1.0]},
                {"index": 0, "embedding": [1.0, 0.0]}
            ]}),
            200,
        );
        let client = Client::new(base.clone(), "key".to_string());
        assert!(client.healthy());
        let vectors = client.embed(&["a".to_string(), "b".to_string()]).unwrap();
        assert_eq!(vectors, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
        assert!(
            !Client::new(base, "wrong".to_string()).healthy(),
            "the key is checked"
        );

        let broken = fake(
            json!({"data": [{"index": 0, "embedding": [0.0, 0.0]}]}),
            200,
        );
        let error = Client::new(broken, "key".to_string())
            .embed(&["a".to_string()])
            .unwrap_err();
        assert!(error.to_string().contains("broken vector"), "{error:#}");

        let refused = fake(json!({"error": {"message": "input is too large"}}), 500);
        let error = Client::new(refused, "key".to_string())
            .embed(&["a".to_string()])
            .unwrap_err();
        assert!(
            error.to_string().contains("input is too large"),
            "{error:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_server_that_exits_says_why() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("glyphst-embed-server-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("llama-server");
        std::fs::write(
            &exe,
            "#!/bin/sh\nif [ \"$1\" = --list-devices ]; then echo 'Available devices:'; echo '  Vulkan0: Test GPU'; exit 0; fi\necho \"error: failed to load model '$2'\" >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let config = ServerConfig {
            exe,
            model: dir.join("model.gguf"),
            library_dirs: Vec::new(),
            pid_file: Some(dir.join("server.pid")),
        };
        assert_eq!(list_devices(&config), vec!["Vulkan0: Test GPU"]);
        match Server::start(&config) {
            Err(ServerError::Exited(log)) => {
                assert!(log.contains("failed to load model"), "{log}");
            }
            Err(other) => panic!("unexpected error: {other}"),
            Ok(_) => panic!("the fake server cannot serve"),
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

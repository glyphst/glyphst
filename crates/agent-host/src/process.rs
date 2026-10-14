//! Agent processes: started in their own process group with piped stdio,
//! read line by line on a thread, and stopped with SIGTERM, then SIGKILL,
//! for the whole group (agents start shells and tools of their own).

use std::{
    collections::VecDeque,
    io::{BufRead as _, BufReader, Read},
    os::unix::process::CommandExt as _,
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};

/// Lines of stderr kept to explain a failure.
const STDERR_LINES: usize = 12;
const STDERR_BYTES: usize = 4096;
/// How long a stopped agent has before it is killed.
const KILL_GRACE: Duration = Duration::from_secs(3);

/// A running agent process.
pub struct AgentProcess {
    child: Child,
    pub stdin: Option<ChildStdin>,
    /// Stdout, a line at a time; closed when the process closes it.
    pub lines: Receiver<String>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    name: String,
}

impl AgentProcess {
    /// Starts `command` with piped stdio in its own process group.
    pub fn spawn(mut command: Command, name: &str) -> Result<Self> {
        // Agents run inside the user's coding agent during development; the
        // nested CLI must not think it is a child of that session.
        for variable in [
            "CLAUDECODE",
            "CLAUDE_CODE_ENTRYPOINT",
            "CLAUDE_CODE_SSE_PORT",
        ] {
            command.env_remove(variable);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command
            .spawn()
            .with_context(|| format!("could not start {name}"))?;
        let stdout = child.stdout.take().context("no stdout")?;
        let stderr = child.stderr.take().context("no stderr")?;
        let (sender, lines) = mpsc::channel();
        thread::Builder::new()
            .name(format!("{name}-stdout"))
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            })?;
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        let kept = tail.clone();
        thread::Builder::new()
            .name(format!("{name}-stderr"))
            .spawn(move || keep_tail(stderr, &kept))?;
        Ok(Self {
            stdin: child.stdin.take(),
            child,
            lines,
            stderr: tail,
            name: name.to_string(),
        })
    }

    /// The end of the process's stderr, to explain a failure.
    pub fn stderr_tail(&self) -> String {
        let lines = self
            .stderr
            .lock()
            .map(|lines| lines.iter().cloned().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default();
        let start = lines.len().saturating_sub(STDERR_BYTES);
        let start = (start..lines.len())
            .find(|index| lines.is_char_boundary(*index))
            .unwrap_or(lines.len());
        lines[start..].to_string()
    }

    /// An error for a process that ended without finishing its turn.
    pub fn exit_error(&mut self) -> anyhow::Error {
        let status = self
            .child
            .wait()
            .map(|status| status.to_string())
            .unwrap_or_else(|error| error.to_string());
        let tail = self.stderr_tail();
        if tail.trim().is_empty() {
            anyhow::anyhow!("{} stopped ({status})", self.name)
        } else {
            anyhow::anyhow!("{} stopped ({status}): {}", self.name, tail.trim())
        }
    }

    /// Closes stdin and waits for the process to end, then stops its group.
    pub fn finish(mut self) {
        self.stdin = None;
        let deadline = Instant::now() + KILL_GRACE;
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        self.stop();
    }

    /// Stops the process and everything it started: SIGTERM to the group,
    /// then SIGKILL after a grace period.
    pub fn stop(&mut self) {
        let group = self.child.id();
        signal_group(group, "TERM");
        let deadline = Instant::now() + KILL_GRACE;
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                // Children left in the group still get the KILL below.
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        signal_group(group, "KILL");
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for AgentProcess {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            signal_group(self.child.id(), "KILL");
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Sends a signal to a process group with `kill` (the workspace allows no
/// unsafe code for libc).
fn signal_group(group: u32, signal: &str) {
    let _ = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg("--")
        .arg(format!("-{group}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn keep_tail(stderr: impl Read, tail: &Mutex<VecDeque<String>>) {
    for line in BufReader::new(stderr).lines() {
        let Ok(line) = line else { break };
        if let Ok(mut tail) = tail.lock() {
            if tail.len() == STDERR_LINES {
                tail.pop_front();
            }
            tail.push_back(line);
        }
    }
}

/// A command for `program` with `cwd`, with the program's own directory on
/// PATH first (npm and nix wrappers start their interpreter by name).
pub fn command(program: &Path, cwd: &Path) -> Command {
    let mut command = Command::new(program);
    command.current_dir(cwd);
    let mut paths = program
        .parent()
        .map(|directory| vec![directory.to_path_buf()])
        .unwrap_or_default();
    paths.extend(crate::executable::search_path());
    if let Ok(path) = std::env::join_paths(paths) {
        command.env("PATH", path);
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processes_stream_lines_and_stop_with_their_children() {
        let mut command = Command::new("sh");
        command.args(["-c", "echo one; echo oops >&2; sleep 30 & echo two; wait"]);
        let mut process = AgentProcess::spawn(command, "test agent").unwrap();
        assert_eq!(process.lines.recv().unwrap(), "one");
        assert_eq!(process.lines.recv().unwrap(), "two");
        let started = Instant::now();
        process.stop();
        assert!(started.elapsed() < Duration::from_secs(4));
        // The stdout thread ends once every writer in the group is gone.
        assert!(process.lines.recv_timeout(Duration::from_secs(2)).is_err());
        assert!(process.stderr_tail().contains("oops"));
    }
}

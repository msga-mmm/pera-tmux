use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio::time::{self, MissedTickBehavior};

const FOCUSED_PANE_FORMAT: &str = "#{pane_id}\t#{window_id}\t#{session_name}\t#{pane_active}\t#{pane_title}\t#{pane_current_path}\t#{pane_current_command}";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    pub id: String,
    pub window_id: String,
    pub session_name: String,
    pub active: bool,
    pub title: String,
    pub current_path: PathBuf,
    pub current_command: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Event {
    PaneFocusIn,
}

#[derive(Debug)]
pub enum TmuxError {
    BinaryNotFound,
    CommandFailed { command: String, stderr: String },
    InvalidPanePayload(String),
    Io(std::io::Error),
}

#[derive(Clone)]
pub struct Tmux {
    inner: Arc<Inner>,
}

impl std::fmt::Display for TmuxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TmuxError::BinaryNotFound => write!(f, "tmux binary not found on PATH"),
            TmuxError::CommandFailed { command, stderr } => {
                write!(f, "tmux command failed (`{command}`): {stderr}")
            }
            TmuxError::InvalidPanePayload(payload) => {
                write!(f, "invalid pane payload from tmux: {payload}")
            }
            TmuxError::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for TmuxError {}

impl From<std::io::Error> for TmuxError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

struct Inner {
    binary: String,
    poll_interval: Duration,
    listeners: Mutex<Vec<JoinHandle<()>>>,
}

impl std::fmt::Debug for Tmux {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tmux")
            .field("binary", &self.inner.binary)
            .field("poll_interval", &self.inner.poll_interval)
            .finish()
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Ok(mut listeners) = self.listeners.lock() {
            for handle in listeners.drain(..) {
                handle.abort();
            }
        }
    }
}

impl Tmux {
    pub async fn connect() -> Result<Self, TmuxError> {
        Self::connect_with_binary("tmux").await
    }

    pub async fn connect_with_binary(binary: impl Into<String>) -> Result<Self, TmuxError> {
        let tmux = Self {
            inner: Arc::new(Inner {
                binary: binary.into(),
                poll_interval: Duration::from_millis(250),
                listeners: Mutex::new(Vec::new()),
            }),
        };

        tmux.run_tmux(["-V"]).await?;
        Ok(tmux)
    }

    pub fn set_poll_interval(&mut self, poll_interval: Duration) {
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            inner.poll_interval = poll_interval;
            return;
        }

        let binary = self.inner.binary.clone();
        let listeners = Mutex::new(Vec::new());
        self.inner = Arc::new(Inner {
            binary,
            poll_interval,
            listeners,
        });
    }

    pub async fn focused_pane(&self) -> Result<Pane, TmuxError> {
        let out = self
            .run_tmux(["display-message", "-p", "-F", FOCUSED_PANE_FORMAT])
            .await?;
        parse_pane_line(out.trim())
    }

    pub fn on<F>(&self, event: Event, handler: F)
    where
        F: Fn(Pane) + Send + Sync + 'static,
    {
        let tmux = self.clone();
        let handle = match event {
            Event::PaneFocusIn => tokio::spawn(async move {
                let mut ticker = time::interval(tmux.inner.poll_interval);
                ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
                let mut last_focused: Option<String> = None;

                loop {
                    ticker.tick().await;
                    if let Ok(pane) = tmux.focused_pane().await {
                        if last_focused.as_ref() != Some(&pane.id) {
                            last_focused = Some(pane.id.clone());
                            handler(pane);
                        }
                    }
                }
            }),
        };

        if let Ok(mut listeners) = self.inner.listeners.lock() {
            listeners.push(handle);
        }
    }

    async fn run_tmux<const N: usize>(&self, args: [&str; N]) -> Result<String, TmuxError> {
        let output = Command::new(&self.inner.binary)
            .args(args)
            .output()
            .await
            .map_err(|err| match err.kind() {
                ErrorKind::NotFound => TmuxError::BinaryNotFound,
                _ => TmuxError::Io(err),
            })?;

        if !output.status.success() {
            let command = format!("{} {}", self.inner.binary, args.join(" "));
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(TmuxError::CommandFailed { command, stderr });
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

fn parse_pane_line(line: &str) -> Result<Pane, TmuxError> {
    let fields: Vec<&str> = line.splitn(7, '\t').collect();
    if fields.len() != 7 {
        return Err(TmuxError::InvalidPanePayload(line.to_string()));
    }

    let active = match fields[3] {
        "0" => false,
        "1" => true,
        _ => return Err(TmuxError::InvalidPanePayload(line.to_string())),
    };

    Ok(Pane {
        id: fields[0].to_string(),
        window_id: fields[1].to_string(),
        session_name: fields[2].to_string(),
        active,
        title: fields[4].to_string(),
        current_path: PathBuf::from(fields[5]),
        current_command: fields[6].to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::parse_pane_line;

    #[test]
    fn parse_pane_line_works() {
        let line = "%1\t@3\tdev\t1\teditor\t/project/workspace\tnvim";
        let pane = parse_pane_line(line).expect("valid pane payload");
        assert_eq!(pane.id, "%1");
        assert!(pane.active);
        assert_eq!(pane.current_command, "nvim");
    }
}

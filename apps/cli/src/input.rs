// These blocking reads run on a detached OS thread so timed-out stdin/FIFO operations cannot
// hold up the Tokio runtime or keep the CLI process alive during shutdown.
#![allow(clippy::disallowed_types)]

use std::{
    fs::File,
    io::{self, IsTerminal, Read},
    path::{Path, PathBuf},
    time::Duration,
};

use jev_core::MAX_STATE_BYTES;
use tokio::sync::oneshot;

use crate::args::Cli;

#[derive(Debug)]
pub enum InputSource {
    Inline(String),
    File(PathBuf),
    Stdin,
}

impl InputSource {
    pub fn resolve(cli: &Cli) -> Result<Self, String> {
        if cli.state.is_some() && cli.state_file.is_some() {
            return Err("provide either --state or --state-file, not both".to_owned());
        }
        if let Some(state) = &cli.state {
            if state.len() > MAX_STATE_BYTES {
                return Err(format!(
                    "--state is {0} bytes; the shared state limit is {MAX_STATE_BYTES} bytes",
                    state.len()
                ));
            }
            return Ok(Self::Inline(state.clone()));
        }
        if let Some(path) = &cli.state_file {
            return if path == Path::new("-") {
                Ok(Self::Stdin)
            } else {
                Ok(Self::File(path.clone()))
            };
        }
        if !io::stdin().is_terminal() {
            return Ok(Self::Stdin);
        }
        Err("provide --state, --state-file, or pipe state on stdin".to_owned())
    }

    pub async fn acquire(self, timeout: Duration) -> Result<String, String> {
        match self {
            Self::Inline(value) => Ok(value),
            source => {
                let (sender, receiver) = oneshot::channel();
                // A detached OS thread is deliberate: a blocking stdin/FIFO open cannot be
                // cancelled, and must not keep runtime shutdown alive after the deadline.
                std::thread::Builder::new()
                    .name("jev-state-input".to_owned())
                    .spawn(move || {
                        let result = match source {
                            Self::File(path) => read_file(&path),
                            Self::Stdin => read_stdin(),
                            Self::Inline(_) => Err("invalid input source".to_owned()),
                        };
                        let _ = sender.send(result);
                    })
                    .map_err(|_| "could not start state input reader".to_owned())?;

                match tokio::time::timeout(timeout, receiver).await {
                    Ok(Ok(result)) => result,
                    Ok(Err(_)) => Err("state input reader stopped unexpectedly".to_owned()),
                    Err(_) => Err(format!(
                        "state input exceeded the {} second deadline",
                        timeout.as_secs()
                    )),
                }
            }
        }
    }
}

fn read_file(path: &Path) -> Result<String, String> {
    let file = File::open(path).map_err(|_| "could not open --state-file".to_owned())?;
    read_limited(file, "--state-file")
}

fn read_stdin() -> Result<String, String> {
    read_limited(io::stdin().lock(), "stdin")
}

fn read_limited(reader: impl Read, source: &str) -> Result<String, String> {
    let limit =
        u64::try_from(MAX_STATE_BYTES + 1).map_err(|_| "state byte limit is invalid".to_owned())?;
    let mut bytes = Vec::with_capacity(8_192);
    reader
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(|_| format!("could not read state from {source}"))?;
    if bytes.len() > MAX_STATE_BYTES {
        return Err(format!(
            "state from {source} exceeds the {MAX_STATE_BYTES}-byte limit (read at least {} bytes)",
            bytes.len()
        ));
    }
    String::from_utf8(bytes).map_err(|_| format!("state from {source} is not valid UTF-8"))
}

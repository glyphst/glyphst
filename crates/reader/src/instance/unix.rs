use std::{
    ffi::OsString,
    fs,
    io::ErrorKind,
    os::unix::{ffi::OsStringExt, net::UnixDatagram},
    path::{Path, PathBuf},
    thread,
};

use anyhow::{Context as _, Result};
use futures::channel::mpsc::UnboundedSender;

use super::OpenRequest;

pub(super) fn try_forward(request: &OpenRequest) -> Result<bool> {
    let path = socket_path();
    let socket = UnixDatagram::unbound().context("failed to create IPC socket")?;

    match socket.connect(&path) {
        Ok(()) => {
            let payload = serde_json::to_vec(request).context("failed to encode open request")?;
            match socket.send(&payload) {
                Ok(_) => Ok(true),
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::ConnectionRefused | ErrorKind::NotFound
                    ) =>
                {
                    remove_socket_path(&path)?;
                    Ok(false)
                }
                Err(error) => Err(error).context("failed to forward open request"),
            }
        }
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::ConnectionRefused | ErrorKind::NotFound
            ) =>
        {
            if error.kind() == ErrorKind::ConnectionRefused {
                remove_socket_path(&path)?;
            }
            Ok(false)
        }
        Err(error) => {
            Err(error).with_context(|| format!("failed to connect to {}", path.display()))
        }
    }
}

pub(super) fn listen(sender: UnboundedSender<OpenRequest>) -> Result<()> {
    let path = socket_path();
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    remove_stale_socket(&path)?;
    let listener =
        UnixDatagram::bind(&path).with_context(|| format!("failed to bind {}", path.display()))?;

    thread::spawn(move || {
        let mut buffer = [0; 65536];
        while let Ok(len) = listener.recv(&mut buffer) {
            match serde_json::from_slice::<OpenRequest>(&buffer[..len]) {
                Ok(request) => {
                    if sender.unbounded_send(request).is_err() {
                        break;
                    }
                }
                Err(error) => eprintln!("failed to decode open request: {error:#}"),
            }
        }
    });

    Ok(())
}

pub(super) fn remove_socket_file() -> Result<()> {
    remove_socket_path(&socket_path())
}

pub(super) fn percent_decode_path(path: &str) -> OsString {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut ix = 0;

    while ix < bytes.len() {
        if bytes[ix] == b'%'
            && let (Some(high), Some(low)) = (
                bytes.get(ix + 1).and_then(|byte| hex_value(*byte)),
                bytes.get(ix + 2).and_then(|byte| hex_value(*byte)),
            )
        {
            decoded.push(high * 16 + low);
            ix += 3;
            continue;
        }

        decoded.push(bytes[ix]);
        ix += 1;
    }

    OsString::from_vec(decoded)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn socket_path() -> PathBuf {
    dirs::runtime_dir()
        .unwrap_or_else(glyphst_core::paths::state_dir)
        .join("glyphst-reader.sock")
}

fn remove_stale_socket(path: &Path) -> Result<()> {
    let socket = UnixDatagram::unbound().context("failed to create IPC socket")?;
    match socket.connect(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::ConnectionRefused => remove_socket_path(path),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("failed to connect to {}", path.display()))
        }
    }
}

fn remove_socket_path(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

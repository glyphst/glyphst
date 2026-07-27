use std::{
    ffi::{OsStr, OsString},
    path::PathBuf,
};

use anyhow::Result;
use futures::channel::mpsc::UnboundedSender;
use serde::{Deserialize, Serialize};

#[cfg(unix)]
mod unix;
#[cfg(not(unix))]
mod windows;

#[cfg(unix)]
use unix as platform;
#[cfg(not(unix))]
use windows as platform;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct OpenRequest {
    pub(crate) new: bool,
    pub(crate) path: Option<PathBuf>,
}

pub(crate) fn parse_args() -> OpenRequest {
    let mut new = false;
    let mut path = None;
    let mut args = std::env::args_os().skip(1);

    while let Some(arg) = args.next() {
        if arg.as_os_str() == OsStr::new("--new") {
            new = true;
            continue;
        }

        if arg.as_os_str() == OsStr::new("--") {
            if path.is_none() {
                path = args.next().map(path_from_arg);
            }
            break;
        }

        if path.is_none() {
            path = Some(path_from_arg(arg));
        }
    }

    OpenRequest { new, path }
}

pub(crate) fn try_forward(request: &OpenRequest) -> Result<bool> {
    platform::try_forward(request)
}

pub(crate) fn listen(sender: UnboundedSender<OpenRequest>) -> Result<()> {
    platform::listen(sender)
}

pub(crate) fn remove_socket_file() -> Result<()> {
    platform::remove_socket_file()
}

fn path_from_arg(arg: OsString) -> PathBuf {
    let Some(arg_text) = arg.to_str() else {
        return PathBuf::from(arg);
    };

    if let Some(file_uri) = arg_text.strip_prefix("file://") {
        let path = file_uri.strip_prefix("localhost").unwrap_or(file_uri);
        return PathBuf::from(platform::percent_decode_path(path));
    }

    PathBuf::from(arg)
}

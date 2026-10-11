use std::{
    ffi::{OsStr, OsString},
    path::PathBuf,
};

use anyhow::{Result, bail};
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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct OpenRequest {
    pub(crate) new: bool,
    pub(crate) path: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StartupRequest {
    pub(crate) open: OpenRequest,
}

pub(crate) fn parse_args() -> Result<StartupRequest> {
    parse_args_from(std::env::args_os().skip(1))
}

fn parse_args_from(args: impl IntoIterator<Item = OsString>) -> Result<StartupRequest> {
    let mut new = false;
    let mut path = None;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        if arg.as_os_str() == OsStr::new("--new") {
            new = true;
            continue;
        }

        if arg.as_os_str() == OsStr::new("--") {
            if path.is_none() {
                path = args.next().map(path_from_arg);
            }
            if args.next().is_some() {
                bail!("only one document path may be opened at startup");
            }
            break;
        }

        if path.is_none() {
            path = Some(path_from_arg(arg));
        } else {
            bail!("only one document path may be opened at startup");
        }
    }

    Ok(StartupRequest {
        open: OpenRequest { new, path },
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<StartupRequest> {
        parse_args_from(arguments.iter().map(OsString::from))
    }

    #[test]
    fn ordinary_open_request_remains_forwardable() {
        let request = parse(&["--new", "file:///tmp/a%20book.pdf"]).unwrap();
        assert_eq!(
            request.open,
            OpenRequest {
                new: true,
                path: Some(PathBuf::from("/tmp/a book.pdf")),
            }
        );
    }

    #[test]
    fn rejects_a_second_startup_path() {
        let error = parse(&["/tmp/a.pdf", "/tmp/b.pdf"]).unwrap_err();
        assert!(error.to_string().contains("only one document path"));
    }
}

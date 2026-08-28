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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RenderTraceKind {
    Scroll,
    Zoom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RenderTraceEngine {
    MuPdf,
    Glyphst,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RenderTraceRequest {
    pub(crate) kind: RenderTraceKind,
    pub(crate) engine: RenderTraceEngine,
    pub(crate) page: usize,
    pub(crate) output: PathBuf,
    pub(crate) window_width: f32,
    pub(crate) window_height: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StartupRequest {
    pub(crate) open: OpenRequest,
    pub(crate) trace: Option<RenderTraceRequest>,
}

pub(crate) fn parse_args() -> Result<StartupRequest> {
    parse_args_from(std::env::args_os().skip(1))
}

fn parse_args_from(args: impl IntoIterator<Item = OsString>) -> Result<StartupRequest> {
    let mut new = false;
    let mut path = None;
    let mut trace_kind = None;
    let mut trace_engine = None;
    let mut trace_page = 1_usize;
    let mut trace_output = None;
    let mut window_width = 1440.0_f32;
    let mut window_height = 900.0_f32;
    let mut trace_option_seen = false;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        if arg.as_os_str() == OsStr::new("--new") {
            new = true;
            continue;
        }

        if arg.as_os_str() == OsStr::new("--render-trace") {
            trace_option_seen = true;
            let value = required_option_value(&mut args, "--render-trace")?;
            trace_kind = Some(match value.to_str() {
                Some("scroll") => RenderTraceKind::Scroll,
                Some("zoom") => RenderTraceKind::Zoom,
                _ => bail!("--render-trace must be 'scroll' or 'zoom'"),
            });
            continue;
        }

        if arg.as_os_str() == OsStr::new("--render-engine") {
            trace_option_seen = true;
            let value = required_option_value(&mut args, "--render-engine")?;
            trace_engine = Some(match value.to_str() {
                Some("mupdf") => RenderTraceEngine::MuPdf,
                Some("glyphst") => RenderTraceEngine::Glyphst,
                _ => bail!("--render-engine must be 'mupdf' or 'glyphst'"),
            });
            continue;
        }

        if arg.as_os_str() == OsStr::new("--render-page") {
            trace_option_seen = true;
            let value = required_option_value(&mut args, "--render-page")?;
            trace_page = parse_positive_usize(&value, "--render-page")?;
            continue;
        }

        if arg.as_os_str() == OsStr::new("--render-output") {
            trace_option_seen = true;
            trace_output = Some(PathBuf::from(required_option_value(
                &mut args,
                "--render-output",
            )?));
            continue;
        }

        if arg.as_os_str() == OsStr::new("--window-width") {
            trace_option_seen = true;
            let value = required_option_value(&mut args, "--window-width")?;
            window_width = parse_window_extent(&value, "--window-width", 900.0)?;
            continue;
        }

        if arg.as_os_str() == OsStr::new("--window-height") {
            trace_option_seen = true;
            let value = required_option_value(&mut args, "--window-height")?;
            window_height = parse_window_extent(&value, "--window-height", 600.0)?;
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

    let trace = if trace_option_seen {
        if new {
            bail!("--new cannot be combined with an automated render trace");
        }
        let path = path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("an automated render trace requires a PDF path"))?;
        if path.extension().and_then(OsStr::to_str) != Some("pdf") {
            bail!("an automated render trace requires a .pdf document");
        }
        Some(RenderTraceRequest {
            kind: trace_kind
                .ok_or_else(|| anyhow::anyhow!("missing required --render-trace option"))?,
            engine: trace_engine
                .ok_or_else(|| anyhow::anyhow!("missing required --render-engine option"))?,
            page: trace_page,
            output: trace_output
                .ok_or_else(|| anyhow::anyhow!("missing required --render-output option"))?,
            window_width,
            window_height,
        })
    } else {
        None
    };

    Ok(StartupRequest {
        open: OpenRequest { new, path },
        trace,
    })
}

fn required_option_value(
    args: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> Result<OsString> {
    args.next()
        .ok_or_else(|| anyhow::anyhow!("missing value for {option}"))
}

fn parse_positive_usize(value: &OsStr, option: &str) -> Result<usize> {
    let value = value
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("{option} is not valid UTF-8"))?;
    let parsed = value
        .parse::<usize>()
        .map_err(|_| anyhow::anyhow!("{option} must be a positive integer"))?;
    if parsed == 0 {
        bail!("{option} must be a positive integer");
    }
    Ok(parsed)
}

fn parse_window_extent(value: &OsStr, option: &str, minimum: f32) -> Result<f32> {
    let value = value
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("{option} is not valid UTF-8"))?;
    let parsed = value
        .parse::<f32>()
        .map_err(|_| anyhow::anyhow!("{option} must be a finite number"))?;
    if !parsed.is_finite() || parsed < minimum {
        bail!("{option} must be a finite number at least {minimum}");
    }
    Ok(parsed)
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
        assert!(request.trace.is_none());
    }

    #[test]
    fn parses_complete_automated_trace() {
        let request = parse(&[
            "/tmp/book.pdf",
            "--render-trace",
            "zoom",
            "--render-engine",
            "glyphst",
            "--render-page",
            "29",
            "--render-output",
            "/tmp/trace.json",
            "--window-width",
            "1536",
            "--window-height",
            "960",
        ])
        .unwrap();
        assert_eq!(
            request.trace,
            Some(RenderTraceRequest {
                kind: RenderTraceKind::Zoom,
                engine: RenderTraceEngine::Glyphst,
                page: 29,
                output: PathBuf::from("/tmp/trace.json"),
                window_width: 1536.0,
                window_height: 960.0,
            })
        );
    }

    #[test]
    fn trace_requires_every_identity_option() {
        let error = parse(&[
            "/tmp/book.pdf",
            "--render-trace",
            "scroll",
            "--render-output",
            "/tmp/trace.json",
        ])
        .unwrap_err();
        assert!(error.to_string().contains("--render-engine"));
    }

    #[test]
    fn trace_rejects_zero_page_and_small_window() {
        let zero_page = parse(&[
            "/tmp/book.pdf",
            "--render-trace",
            "zoom",
            "--render-page",
            "0",
        ])
        .unwrap_err();
        assert!(zero_page.to_string().contains("positive integer"));

        let small_window = parse(&[
            "/tmp/book.pdf",
            "--window-width",
            "899",
            "--render-trace",
            "zoom",
        ])
        .unwrap_err();
        assert!(small_window.to_string().contains("at least 900"));
    }

    #[test]
    fn trace_rejects_compatibility_backend_and_non_pdf_input() {
        let compatibility = parse(&[
            "/tmp/book.pdf",
            "--render-trace",
            "zoom",
            "--render-engine",
            "glyphst-first",
        ])
        .unwrap_err();
        assert!(compatibility.to_string().contains("mupdf' or 'glyphst"));

        let non_pdf = parse(&[
            "/tmp/book.epub",
            "--render-trace",
            "zoom",
            "--render-engine",
            "glyphst",
            "--render-output",
            "/tmp/trace.json",
        ])
        .unwrap_err();
        assert!(non_pdf.to_string().contains(".pdf"));
    }
}

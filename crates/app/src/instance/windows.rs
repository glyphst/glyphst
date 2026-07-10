use std::ffi::OsString;

use anyhow::Result;
use futures::channel::mpsc::UnboundedSender;

use super::OpenRequest;

pub(super) fn try_forward(_: &OpenRequest) -> Result<bool> {
    Ok(false)
}

pub(super) fn listen(_: UnboundedSender<OpenRequest>) -> Result<()> {
    Ok(())
}

pub(super) fn remove_socket_file() -> Result<()> {
    Ok(())
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

    String::from_utf8_lossy(&decoded).into_owned().into()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

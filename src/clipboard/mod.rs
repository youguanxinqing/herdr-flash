mod error;

pub use error::ClipboardError;

use std::io::{self, Write};

/// Clipboard abstraction used by picker code and tests.
pub trait Clipboard {
    fn copy(&self, text: &str) -> Result<(), ClipboardError>;
}

/// System clipboard implementation over OSC 52.
///
/// The picker always runs inside a Herdr pane, and Herdr relays a pane's OSC 52 write to the
/// terminal it is attached to. That beats spawning `pbcopy`/`xclip` on two counts. It reaches the
/// clipboard of whatever machine the human is sitting at, so a pane opened through
/// `herdr --remote` copies to the local clipboard instead of the remote host's. And it survives a
/// long-lived Herdr server whose macOS launchd session died under it — an `exec`ed `pbcopy` there
/// inherits the dead Mach bootstrap and cannot reach `pboard`, while the relay runs in the
/// attached client process, which is always in the live session.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClipboard;

impl Clipboard for SystemClipboard {
    fn copy(&self, text: &str) -> Result<(), ClipboardError> {
        write_osc52(&mut io::stdout(), text)
    }
}

/// Writes one OSC 52 clipboard-set sequence and flushes it to the terminal.
///
/// ponytail: no chunking and no size cap. The picker can only ever select visible pane text, so
/// the payload is a screenful at most; if it ever copies scrollback, terminals start truncating
/// around 100KB and this needs splitting.
fn write_osc52(out: &mut impl Write, text: &str) -> Result<(), ClipboardError> {
    let failed = |error: io::Error| ClipboardError::WriteFailed {
        message: error.to_string(),
    };
    write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes())).map_err(failed)?;
    out.flush().map_err(failed)
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let packed = u32::from(chunk[0]) << 16
            | u32::from(chunk.get(1).copied().unwrap_or(0)) << 8
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        for index in 0..4 {
            // A 2-byte chunk carries 3 sextets and a 1-byte chunk carries 2; the rest is padding.
            encoded.push(if index <= chunk.len() {
                ALPHABET[(packed >> (18 - 6 * index)) as usize & 0x3f] as char
            } else {
                '='
            });
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_pads_every_chunk_remainder() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"osc52-works"), "b3NjNTItd29ya3M=");
        // Bytes above 0x7f must survive: pane text is UTF-8, not ASCII.
        assert_eq!(base64("héllo".as_bytes()), "aMOpbGxv");
    }

    #[test]
    fn writes_one_osc52_set_sequence() {
        let mut out = Vec::new();

        write_osc52(&mut out, "osc52-works").unwrap();

        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\x1b]52;c;b3NjNTItd29ya3M=\x07"
        );
    }
}

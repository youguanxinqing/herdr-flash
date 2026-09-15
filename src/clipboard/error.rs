use thiserror::Error;

/// User-facing failure while copying text to the system clipboard.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ClipboardError {
    #[error("failed to write the OSC 52 clipboard sequence to the terminal: {message}")]
    WriteFailed { message: String },
}

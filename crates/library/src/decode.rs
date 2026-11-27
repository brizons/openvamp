//! What stem decoding shares across formats: the error both formats give,
//! and the choice of format from a file's first bytes. WAV stems are read
//! with `hound` in the crate root and the disk thread; FLAC stems with
//! `symphonia` in [`flac`].

use std::fs::File;
use std::io::{self, Read, Seek};

use thiserror::Error;

mod flac;

pub(crate) use flac::FlacStem;

/// The first bytes of every FLAC file.
const FLAC_MARKER: &[u8; 4] = b"fLaC";

/// Why the audio of a stem file could not be decoded, whatever its format.
/// The message is the decoder's own, so a WAV stem says what it always said.
#[derive(Debug, Error)]
pub enum DecodeError {
    /// The file could not be read.
    #[error(transparent)]
    Io(io::Error),
    /// The file is not well formed, ends before its declared length, or uses
    /// something the decoder cannot read.
    #[error("{0}")]
    Malformed(String),
}

impl From<hound::Error> for DecodeError {
    fn from(error: hound::Error) -> Self {
        match error {
            hound::Error::IoError(error) => Self::Io(error),
            other => Self::Malformed(other.to_string()),
        }
    }
}

impl From<symphonia::core::errors::Error> for DecodeError {
    fn from(error: symphonia::core::errors::Error) -> Self {
        match error {
            symphonia::core::errors::Error::IoError(error) => Self::Io(error),
            other => Self::Malformed(other.to_string()),
        }
    }
}

/// Whether `file` starts with the FLAC marker; any other file is read as WAV.
/// The file is left at its start.
pub(crate) fn starts_as_flac(file: &mut File) -> io::Result<bool> {
    let mut head = Vec::with_capacity(FLAC_MARKER.len());
    file.by_ref()
        .take(FLAC_MARKER.len() as u64)
        .read_to_end(&mut head)?;
    file.rewind()?;
    Ok(head == FLAC_MARKER)
}

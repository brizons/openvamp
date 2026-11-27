//! A FLAC file as the reader is given it: the marker, the STREAMINFO block
//! as the only metadata, then the file's audio frames.
//!
//! The reader never sees the file's other metadata blocks. symphonia 0.6.1
//! subtracts SEEKTABLE byte offsets without checking them, so a point past
//! the end of a cut or forged file panics in a debug build and seeks wrong in
//! a release one; a stem needs no tags, pictures or cue sheet; and without
//! them, the fresh reader every seek opens reads 42 bytes of metadata.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};

use symphonia::core::io::MediaSource;

/// Bytes of STREAMINFO.
const STREAMINFO_LEN: usize = 34;

/// Bytes of the header the reader is given: the marker, a block header and
/// STREAMINFO.
const HEADER_LEN: usize = 4 + 4 + STREAMINFO_LEN;

/// The file holds no audio frame after its metadata.
pub(super) const ENDS_BEFORE_AUDIO: &str =
    "Ill-formed FLAC file: the file ends before its first audio frame";

/// Where the audio frames of a FLAC file start, and the header the reader
/// is given in front of them.
#[derive(Debug, Clone, Copy)]
pub(super) struct Layout {
    header: [u8; HEADER_LEN],
    /// Byte offset of the first byte after the metadata blocks.
    audio_start: u64,
    /// Size of the file in bytes.
    file_len: u64,
}

impl Layout {
    /// Reads the metadata block headers of `file`, which starts with the
    /// FLAC marker and holds `file_len` bytes, and its STREAMINFO block; why
    /// not, in words, when the blocks do not fit in the file or STREAMINFO
    /// is missing, repeated or not 34 bytes.
    pub(super) fn read(file: &File, file_len: u64) -> Result<Self, String> {
        let io_error = |error: io::Error| match error.kind() {
            io::ErrorKind::UnexpectedEof => ENDS_BEFORE_AUDIO.to_owned(),
            _ => error.to_string(),
        };
        let mut reader = BufReader::new(file);
        reader.seek(SeekFrom::Start(4)).map_err(io_error)?;
        let mut at = 4;
        let mut stream_info = None;
        loop {
            let mut block = [0; 4];
            reader.read_exact(&mut block).map_err(io_error)?;
            let len = u64::from(u32::from_be_bytes([0, block[1], block[2], block[3]]));
            at += 4;
            if at + len > file_len {
                return Err(
                    "Ill-formed FLAC file: a metadata block runs past the end of the file"
                        .to_owned(),
                );
            }
            if block[0] & 0x7F == 0 {
                if stream_info.is_some() {
                    return Err(
                        "Ill-formed FLAC file: it has more than one STREAMINFO block".to_owned(),
                    );
                }
                if len != STREAMINFO_LEN as u64 {
                    return Err(
                        "Ill-formed FLAC file: its STREAMINFO block is not 34 bytes".to_owned()
                    );
                }
                let mut info = [0; STREAMINFO_LEN];
                reader.read_exact(&mut info).map_err(io_error)?;
                stream_info = Some(info);
            } else {
                // At most 2^24 - 1 bytes.
                reader.seek_relative(len as i64).map_err(io_error)?;
            }
            at += len;
            if block[0] & 0x80 != 0 {
                break;
            }
        }
        let info = stream_info
            .ok_or_else(|| "Ill-formed FLAC file: it has no STREAMINFO block".to_owned())?;
        let mut header = [0; HEADER_LEN];
        header[..4].copy_from_slice(b"fLaC");
        // The last metadata block, of type STREAMINFO (0), 34 bytes long.
        header[4..8].copy_from_slice(&[0x80, 0, 0, STREAMINFO_LEN as u8]);
        header[8..].copy_from_slice(&info);
        Ok(Self {
            header,
            audio_start: at,
            file_len,
        })
    }

    /// The audio frames of `file` behind the header.
    pub(super) fn source(self, file: File) -> AudioFrames {
        AudioFrames {
            layout: self,
            file,
            position: 0,
        }
    }
}

/// The stream the reader reads: [`Layout`]'s header, then the file from its
/// first byte after the metadata.
pub(super) struct AudioFrames {
    layout: Layout,
    file: File,
    /// Position in the stream.
    position: u64,
}

impl AudioFrames {
    fn len(&self) -> u64 {
        HEADER_LEN as u64 + (self.layout.file_len - self.layout.audio_start)
    }
}

impl Read for AudioFrames {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = match usize::try_from(self.position) {
            Ok(from) if from < HEADER_LEN => {
                let read = buf.len().min(HEADER_LEN - from);
                buf[..read].copy_from_slice(&self.layout.header[from..from + read]);
                read
            }
            _ => {
                let offset = self.layout.audio_start + (self.position - HEADER_LEN as u64);
                self.file.seek(SeekFrom::Start(offset))?;
                self.file.read(buf)?
            }
        };
        self.position += read as u64;
        Ok(read)
    }
}

impl Seek for AudioFrames {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let position = match to {
            SeekFrom::Start(position) => Some(position),
            SeekFrom::End(delta) => self.len().checked_add_signed(delta),
            SeekFrom::Current(delta) => self.position.checked_add_signed(delta),
        };
        self.position = position.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "a seek before the start")
        })?;
        Ok(self.position)
    }
}

impl MediaSource for AudioFrames {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.len())
    }
}

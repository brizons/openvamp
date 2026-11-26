//! The encoder delay and the length an M4A file records in the edit list of
//! its audio track (`moov/trak/edts/elst`), which `symphonia` 0.6 reads but
//! does not apply.
//!
//! An AAC encoder adds priming frames before the audio and pads the last
//! packet; the edit list says which frames play: the first edit that is
//! not empty starts at `media_time` (the delay) and lasts
//! `segment_duration`. ffmpeg writes it that way. A file with several such
//! edits, or none, is left untrimmed.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};

/// What an edit list asks to play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EditList {
    /// Frames to drop at the start.
    pub(super) skip: u64,
    /// Frames to play after them, when the edit says.
    pub(super) keep: Option<u64>,
}

/// Most boxes looked at inside one box, so a damaged file cannot keep the
/// search going.
const MAX_BOXES: usize = 4_096;

/// Most edits read.
const MAX_EDITS: u32 = 16;

/// The edit list of the first audio track of `file`, in frames of its media
/// timescale (the sample rate in M4A files); `None` without one the import
/// can apply.
pub(super) fn edit_list(file: &mut File) -> io::Result<Option<EditList>> {
    let end = file.metadata()?.len();
    let Some(moov) = find(file, 0, end, *b"moov")? else {
        return Ok(None);
    };
    let Some(mvhd) = find(file, moov.start, moov.end, *b"mvhd")? else {
        return Ok(None);
    };
    let movie_timescale = timescale(file, mvhd)?;
    for trak in children(file, moov.start, moov.end)?
        .into_iter()
        .filter(|child| child.kind == *b"trak")
    {
        let Some(mdia) = find(file, trak.start, trak.end, *b"mdia")? else {
            continue;
        };
        let (Some(hdlr), Some(mdhd)) = (
            find(file, mdia.start, mdia.end, *b"hdlr")?,
            find(file, mdia.start, mdia.end, *b"mdhd")?,
        ) else {
            continue;
        };
        // hdlr: version and flags, pre_defined, then the handler type.
        if read_at(file, hdlr.start + 8, 4)? != b"soun" {
            continue;
        }
        let media_timescale = timescale(file, mdhd)?;
        let Some(edts) = find(file, trak.start, trak.end, *b"edts")? else {
            return Ok(None);
        };
        let Some(elst) = find(file, edts.start, edts.end, *b"elst")? else {
            return Ok(None);
        };
        return read_edits(file, elst, movie_timescale, media_timescale);
    }
    Ok(None)
}

/// A box: its type and where its contents start and end.
#[derive(Debug, Clone, Copy)]
struct Mp4Box {
    kind: [u8; 4],
    start: u64,
    end: u64,
}

/// The boxes from `start` to `end`.
fn children(file: &mut File, start: u64, end: u64) -> io::Result<Vec<Mp4Box>> {
    let mut boxes = Vec::new();
    let mut at = start;
    while at + 8 <= end && boxes.len() < MAX_BOXES {
        let header = read_at(file, at, 8)?;
        let size = u64::from(u32::from_be_bytes(header[..4].try_into().expect("4 bytes")));
        let kind: [u8; 4] = header[4..].try_into().expect("4 bytes");
        let (header_len, size) = match size {
            0 => (8, end - at),
            1 => {
                let large = read_at(file, at + 8, 8)?;
                (
                    16,
                    u64::from_be_bytes(large[..].try_into().expect("8 bytes")),
                )
            }
            size => (8, size),
        };
        if size < header_len || at + size > end {
            break;
        }
        boxes.push(Mp4Box {
            kind,
            start: at + header_len,
            end: at + size,
        });
        at += size;
    }
    Ok(boxes)
}

/// The first box of type `kind` from `start` to `end`.
fn find(file: &mut File, start: u64, end: u64, kind: [u8; 4]) -> io::Result<Option<Mp4Box>> {
    Ok(children(file, start, end)?
        .into_iter()
        .find(|child| child.kind == kind))
}

/// The timescale of an `mvhd` or `mdhd` box.
fn timescale(file: &mut File, header: Mp4Box) -> io::Result<u64> {
    let version = read_at(file, header.start, 1)?[0];
    // Version and flags, then creation and modification times of 4 or 8
    // bytes each.
    let offset = if version == 1 { 4 + 16 } else { 4 + 8 };
    let bytes = read_at(file, header.start + offset, 4)?;
    Ok(u64::from(u32::from_be_bytes(
        bytes[..].try_into().expect("4 bytes"),
    )))
}

/// The one edit with media that an `elst` box holds, in media frames.
fn read_edits(
    file: &mut File,
    elst: Mp4Box,
    movie_timescale: u64,
    media_timescale: u64,
) -> io::Result<Option<EditList>> {
    let head = read_at(file, elst.start, 8)?;
    let version = head[0];
    let count = u32::from_be_bytes(head[4..].try_into().expect("4 bytes")).min(MAX_EDITS);
    let entry_len = if version == 1 { 20 } else { 12 };
    let mut with_media = Vec::new();
    for index in 0..u64::from(count) {
        let at = elst.start + 8 + index * entry_len;
        if at + entry_len > elst.end {
            break;
        }
        let entry = read_at(file, at, entry_len as usize)?;
        let (duration, media_time) = if version == 1 {
            (
                u64::from_be_bytes(entry[..8].try_into().expect("8 bytes")),
                i64::from_be_bytes(entry[8..16].try_into().expect("8 bytes")),
            )
        } else {
            (
                u64::from(u32::from_be_bytes(entry[..4].try_into().expect("4 bytes"))),
                i64::from(i32::from_be_bytes(entry[4..8].try_into().expect("4 bytes"))),
            )
        };
        // -1 is an empty edit: a pause before the media plays.
        if media_time >= 0 {
            with_media.push((duration, media_time as u64));
        }
    }
    let [(duration, media_time)] = with_media[..] else {
        return Ok(None);
    };
    let keep = (movie_timescale > 0 && duration > 0).then(|| {
        let frames =
            u128::from(duration) * u128::from(media_timescale) / u128::from(movie_timescale);
        u64::try_from(frames).unwrap_or(u64::MAX)
    });
    Ok(Some(EditList {
        skip: media_time,
        keep,
    }))
}

fn read_at(file: &mut File, at: u64, len: usize) -> io::Result<Vec<u8>> {
    let mut bytes = vec![0; len];
    file.seek(SeekFrom::Start(at))?;
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

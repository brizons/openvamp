//! The waveform overview of a song (S3): the peak level of its `track`
//! stems mixed at unity, [`OVERVIEW_VALUES_PER_SECOND`] values a second,
//! from which a shell draws one bar per musical bar
//! ([`Overview::range_level`]).
//!
//! [`build_overview`] reads the stems with the player's readers, a block at
//! a time, and caches the result in the song's [`WAVEFORMS_FOLDER`].
//! [`load_overview`] reads that cache back while every stem file has the
//! name, size and modified time it had when the cache was built.
//!
//! The [`WAVEFORMS_FOLDER`] is a cache, safe to delete: a missing, stale or
//! damaged cache is rebuilt from the stems, and a cache that cannot be
//! written (a read-only library) only means the next build reads the stems
//! again.

use std::fs;
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use domain::{SongPackage, Stem, StemRole};
use hound::SampleFormat;
use thiserror::Error;

use crate::decode::FlacStem;
use crate::{
    DecodeError, LoadError, MANIFEST_FILE, OpenedStem, StemReader, check_package, int_scale,
    is_plain_relative, read_confined_bytes, resolve_inside, write_file_atomically,
};

/// Name of the folder, inside a song folder, that caches the song's
/// waveforms; safe to delete.
pub const WAVEFORMS_FOLDER: &str = "waveforms";

/// Path of the cached [`Overview`], relative to the song folder.
pub const OVERVIEW_FILE: &str = "waveforms/overview.bin";

/// Values an [`Overview`] holds per second of audio: each value covers
/// `sample_rate / 100` frames, rounded down (480 at 48 kHz, 441 at
/// 44.1 kHz), and at least one.
pub const OVERVIEW_VALUES_PER_SECOND: u32 = 100;

/// The peak level of a song's `track` stems mixed at unity, one value per
/// window of [`Overview::frames_per_value`] frames from the song's start.
///
/// The mix is the sum of the stems' samples, a mono stem on both sides as
/// the player plays it; a value is the largest absolute sample of either
/// channel in its window. Click and guide stems are left out, so a song
/// without `track` stems has a silent overview.
///
/// Levels are stored as 16-bit fractions of the song's [`Overview::peak`],
/// as the cache keeps them, so an overview read back from the cache equals
/// the one built.
#[derive(Debug, Clone, PartialEq)]
pub struct Overview {
    sample_rate: u32,
    frames_per_value: u32,
    length_frames: u64,
    peak: f32,
    values: Vec<u16>,
}

impl Overview {
    /// Frames per second of the song.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Frames each value covers; the last value covers what is left.
    pub fn frames_per_value(&self) -> u32 {
        self.frames_per_value
    }

    /// Song length in frames: the length of its longest stem, of any role.
    pub fn length_frames(&self) -> u64 {
        self.length_frames
    }

    /// The loudest level of the song: the largest value, 0 for silence.
    pub fn peak(&self) -> f32 {
        self.peak
    }

    /// Number of values.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether there are no values: the song has no frames.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// The levels, one per window, from the song's start.
    pub fn levels(&self) -> impl Iterator<Item = f32> + '_ {
        self.values.iter().map(|&value| self.level(value))
    }

    /// The level of the frames in `frames`, such as one bar: the largest
    /// value whose window holds any of them. An empty range, or one past the
    /// song's end, is silent.
    pub fn range_level(&self, frames: Range<u64>) -> f32 {
        let per_value = u64::from(self.frames_per_value);
        let end = frames.end.min(self.length_frames);
        if frames.start >= end {
            return 0.0;
        }
        // Both fit: the values cover the song's frames.
        let first = (frames.start / per_value) as usize;
        let last = end.div_ceil(per_value) as usize;
        let loudest = self.values[first..last].iter().max().copied();
        loudest.map_or(0.0, |value| self.level(value))
    }

    /// The level a stored value stands for.
    fn level(&self, value: u16) -> f32 {
        f32::from(value) / f32::from(u16::MAX) * self.peak
    }

    /// The overview of `peaks`, one level per window, stored as fractions of
    /// the loudest.
    fn from_peaks(sample_rate: u32, length_frames: u64, peaks: &[f32]) -> Self {
        let peak = peaks.iter().copied().fold(0.0, f32::max);
        let values = peaks
            .iter()
            .map(|&level| {
                if peak > 0.0 {
                    (level / peak * f32::from(u16::MAX)).round() as u16
                } else {
                    0
                }
            })
            .collect();
        Self {
            sample_rate,
            frames_per_value: frames_per_value(sample_rate),
            length_frames,
            peak,
            values,
        }
    }
}

/// Frames each overview value covers at `sample_rate`: a hundredth of a
/// second, rounded down, and at least one frame.
fn frames_per_value(sample_rate: u32) -> u32 {
    (sample_rate / OVERVIEW_VALUES_PER_SECOND).max(1)
}

/// Number of values that cover `length_frames`.
fn value_count(length_frames: u64, frames_per_value: u32) -> u64 {
    length_frames.div_ceil(u64::from(frames_per_value))
}

/// How far [`build_overview`] has got, in frames of the song read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverviewProgress {
    /// Frames read so far, every stem in step.
    pub done: u64,
    /// Frames in the song.
    pub total: u64,
}

impl OverviewProgress {
    /// The share done, from 0 to 1.
    pub fn fraction(&self) -> f64 {
        if self.total == 0 {
            1.0
        } else {
            self.done as f64 / self.total as f64
        }
    }
}

/// Why [`build_overview`] gave no overview. Nothing is written to the
/// [`WAVEFORMS_FOLDER`] when one is returned.
#[derive(Debug, Error)]
pub enum OverviewError {
    /// The song folder does not load, as [`crate::load_song`] reports it, or
    /// a stem could not be decoded.
    #[error(transparent)]
    Load(#[from] LoadError),
    /// The build was cancelled.
    #[error("the waveform overview was cancelled")]
    Cancelled,
}

/// The cached overview of the song folder `dir`, or `None` when there is
/// none to use: no cache, a cache in an unknown format or damaged, or one
/// built from stem files that have changed since (by name, size or modified
/// time). No stem is decoded, and nothing is written.
pub fn load_overview(dir: &Path) -> Option<Overview> {
    let folder = fs::canonicalize(dir).ok()?;
    let json = fs::read_to_string(folder.join(MANIFEST_FILE)).ok()?;
    let package = SongPackage::from_json(&json).ok()?;
    let fingerprints = fingerprints(&folder, &package)?;
    let bytes = read_confined_bytes(&folder, OVERVIEW_FILE, MAX_CACHE_BYTES).ok()?;
    let (overview, cached) = cache::decode(&bytes)?;
    (overview.sample_rate == package.sample_rate && cached == fingerprints).then_some(overview)
}

/// Reads the `track` stems of the song folder `dir` into its [`Overview`]
/// and caches it in the song's [`WAVEFORMS_FOLDER`], creating the folder
/// when needed.
///
/// The folder is checked as [`crate::load_song`] checks it. The stems are
/// read in step, a block of frames at a time, with the readers the player
/// uses, keeping no more than a block of audio; `cancel` is checked between
/// blocks and `progress` told after each one. The cache is written
/// atomically after the last block: a build that is cancelled or fails
/// leaves no cache file, and a cache that cannot be written (a read-only
/// library) still gives the overview.
pub fn build_overview(
    dir: &Path,
    progress: &mut dyn FnMut(OverviewProgress),
    cancel: &AtomicBool,
) -> Result<Overview, OverviewError> {
    let manifest_path = dir.join(MANIFEST_FILE);
    let json = fs::read_to_string(&manifest_path).map_err(|source| LoadError::ReadManifest {
        path: manifest_path,
        source,
    })?;
    let package = SongPackage::from_json(&json).map_err(LoadError::from)?;
    let folder = fs::canonicalize(dir).map_err(|source| LoadError::SongFolder {
        path: dir.to_owned(),
        source,
    })?;
    // Taken before any stem is read: a stem that changes during the build
    // makes the cache stale.
    let fingerprints = fingerprints(&folder, &package);
    let (validated, opened) = check_package(&folder, package).map_err(LoadError::Invalid)?;
    let song = validated.song;
    let mut tracks = opened
        .into_iter()
        .filter(|(stem, _)| stem.role == StemRole::Track)
        .map(|(stem, opened)| TrackReader::new(stem, opened))
        .collect::<Result<Vec<_>, _>>()?;

    let length = song.length();
    let per_value = frames_per_value(song.sample_rate());
    // At most a day of audio (the package rules), so the count fits.
    let mut peaks = Vec::with_capacity(value_count(length, per_value) as usize);
    let mut window = Window::default();
    let mut mix = vec![0.0; 2 * BLOCK_FRAMES];
    let mut scratch = vec![0.0; 2 * BLOCK_FRAMES];
    let mut done = 0;
    while done < length {
        if cancel.load(Ordering::Relaxed) {
            return Err(OverviewError::Cancelled);
        }
        let frames = (length - done).min(BLOCK_FRAMES as u64) as usize;
        let mix = &mut mix[..2 * frames];
        mix.fill(0.0);
        for track in &mut tracks {
            track.add_to(done, mix, &mut scratch)?;
        }
        for frame in mix.chunks_exact(2) {
            if let Some(peak) = window.add(frame, per_value) {
                peaks.push(peak);
            }
        }
        done += frames as u64;
        progress(OverviewProgress {
            done,
            total: length,
        });
    }
    if let Some(peak) = window.finish() {
        peaks.push(peak);
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(OverviewError::Cancelled);
    }
    let overview = Overview::from_peaks(song.sample_rate(), length, &peaks);
    if let Some(fingerprints) = fingerprints {
        // A cache that cannot be written is only a slower next build.
        let _ = write_cache(&folder, &overview, &fingerprints);
    }
    Ok(overview)
}

/// Frames read from every stem at a time, as [`crate::read_through`] reads.
const BLOCK_FRAMES: usize = crate::READ_THROUGH_BLOCK;

/// Most bytes a cache file may hold: far more than the values of the
/// longest song the package rules allow (a day at 100 values a second is
/// about 17 MB), so a larger file is not a cache.
const MAX_CACHE_BYTES: u64 = 64 << 20;

/// The peak of the window of frames being read.
#[derive(Default)]
struct Window {
    peak: f32,
    frames: u32,
}

impl Window {
    /// Adds one stereo frame of the mix; returns the window's peak when the
    /// frame completes a window of `per_value` frames. A sample that is not
    /// finite (a damaged float stem) is left out.
    fn add(&mut self, frame: &[f32], per_value: u32) -> Option<f32> {
        let level = frame[0].abs().max(frame[1].abs());
        if level > self.peak && level.is_finite() {
            self.peak = level;
        }
        self.frames += 1;
        (self.frames == per_value).then(|| self.finish()).flatten()
    }

    /// The peak of the frames added since the last window, if any.
    fn finish(&mut self) -> Option<f32> {
        let peak = (self.frames > 0).then_some(self.peak);
        *self = Self::default();
        peak
    }
}

/// A `track` stem read from its start, a block at a time, with the readers
/// the player uses: samples convert as [`crate::load_song`] converts them.
struct TrackReader {
    stem: Stem,
    frames: u64,
    file: TrackFile,
}

enum TrackFile {
    Wav {
        reader: StemReader,
        channels: usize,
        /// `None` for float samples.
        int_scale: Option<f32>,
    },
    Flac(Box<FlacStem>),
}

impl TrackReader {
    /// Rejects a WAV file that holds fewer samples than its header
    /// declares, as [`crate::load_song`] does.
    fn new(stem: Stem, opened: OpenedStem) -> Result<Self, LoadError> {
        let frames = opened.frames();
        let file = match opened {
            OpenedStem::Wav {
                reader,
                max_samples,
            } => {
                if u64::from(reader.len()) > max_samples {
                    return Err(LoadError::Decode {
                        id: stem.id.clone(),
                        file: stem.file.clone(),
                        source: hound::Error::FormatError(
                            "the file holds fewer samples than its header declares",
                        )
                        .into(),
                    });
                }
                let spec = reader.spec();
                TrackFile::Wav {
                    channels: usize::from(spec.channels),
                    int_scale: (spec.sample_format == SampleFormat::Int)
                        .then(|| int_scale(spec.bits_per_sample)),
                    reader,
                }
            }
            OpenedStem::Flac(flac) => TrackFile::Flac(flac),
        };
        Ok(Self { stem, frames, file })
    }

    /// Adds the stereo frames from frame `start`, the one after the frames
    /// read last, to `mix`: a mono stem on both sides, nothing past the
    /// stem's end. `scratch` holds at least as many samples as `mix`.
    fn add_to(
        &mut self,
        start: u64,
        mix: &mut [f32],
        scratch: &mut [f32],
    ) -> Result<(), LoadError> {
        if start >= self.frames {
            return Ok(());
        }
        let frames = ((mix.len() / 2) as u64).min(self.frames - start) as usize;
        let mix = &mut mix[..2 * frames];
        let read = match &mut self.file {
            TrackFile::Wav {
                reader,
                channels,
                int_scale: Some(scale),
            } => {
                let scale = *scale;
                let samples = reader
                    .samples::<i32>()
                    .map(|sample| sample.map(|sample| sample as f32 * scale));
                add_wav(mix, *channels, samples).map_err(DecodeError::from)
            }
            TrackFile::Wav {
                reader,
                channels,
                int_scale: None,
            } => add_wav(mix, *channels, reader.samples::<f32>()).map_err(DecodeError::from),
            TrackFile::Flac(flac) => {
                let scratch = &mut scratch[..2 * frames];
                flac.read(start, scratch).map(|()| {
                    for (mixed, sample) in mix.iter_mut().zip(scratch.iter()) {
                        *mixed += sample;
                    }
                })
            }
        };
        read.map_err(|source| LoadError::Decode {
            id: self.stem.id.clone(),
            file: self.stem.file.clone(),
            source,
        })
    }
}

/// Adds the next frames of a WAV stem's `samples` to the stereo frames of
/// `mix`, a mono stem on both sides.
fn add_wav(
    mix: &mut [f32],
    channels: usize,
    mut samples: impl Iterator<Item = hound::Result<f32>>,
) -> hound::Result<()> {
    let mut next = || {
        samples.next().unwrap_or(Err(hound::Error::FormatError(
            "the file ends before its declared length",
        )))
    };
    for frame in mix.chunks_exact_mut(2) {
        let left = next()?;
        let right = if channels == 1 { left } else { next()? };
        frame[0] += left;
        frame[1] += right;
    }
    Ok(())
}

/// What the cache records of a stem file, to tell whether it changed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    /// The stem's `file`, as in the manifest.
    file: String,
    role: StemRole,
    /// Size in bytes.
    size: u64,
    /// Modified time since the Unix epoch.
    modified_seconds: u64,
    modified_nanos: u32,
}

/// The fingerprints of every stem of `package`, in package order, inside
/// `folder` (canonical); `None` when one cannot be taken (a path out of the
/// folder, a missing file, no modified time), so no cache is used.
fn fingerprints(folder: &Path, package: &SongPackage) -> Option<Vec<Fingerprint>> {
    package
        .stems
        .iter()
        .map(|stem| {
            if !is_plain_relative(&stem.file) {
                return None;
            }
            let path = resolve_inside(folder, &stem.file).ok()??;
            let metadata = fs::metadata(path).ok()?;
            let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
            Some(Fingerprint {
                file: stem.file.clone(),
                role: stem.role,
                size: metadata.len(),
                modified_seconds: modified.as_secs(),
                modified_nanos: modified.subsec_nanos(),
            })
        })
        .collect()
}

/// Writes the cache of `overview` into the song's [`WAVEFORMS_FOLDER`]
/// inside `folder` (canonical), creating it when needed; a folder that
/// resolves outside the song folder is not written into.
fn write_cache(
    folder: &Path,
    overview: &Overview,
    fingerprints: &[Fingerprint],
) -> std::io::Result<()> {
    match fs::create_dir(folder.join(WAVEFORMS_FOLDER)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let Some(waveforms) = resolve_inside(folder, WAVEFORMS_FOLDER)? else {
        return Err(std::io::Error::other(
            "the waveforms folder resolves outside the song folder",
        ));
    };
    let file = Path::new(OVERVIEW_FILE)
        .file_name()
        .expect("the cache path names a file");
    write_file_atomically(&waveforms.join(file), cache::encode(overview, fingerprints))
}

/// The cache file format, version 1. Every number is little-endian:
///
/// | Bytes | Field |
/// |-------|-------|
/// | 4 | magic, `OVWF` |
/// | 4 | format version, `u32`: 1 |
/// | 4 | sample rate, `u32` |
/// | 4 | frames per value, `u32` |
/// | 8 | song length in frames, `u64` |
/// | 4 | peak level, `f32` |
/// | 4 | stem count, `u32`, then per stem in package order: |
/// | 4 + n | the manifest's `file`: its length in bytes, `u32`, then its UTF-8 bytes |
/// | 1 | role: 0 track, 1 click, 2 guide |
/// | 8 | file size in bytes, `u64` |
/// | 8 + 4 | modified time since the Unix epoch: seconds, `u64`, and nanoseconds, `u32` |
/// | 4 | value count, `u32` |
/// | 2 × count | values, `u16` fractions of the peak level (65 535 is the peak) |
/// | 8 | FNV-1a 64-bit hash of every byte before it |
///
/// A file of another magic, version or size, with a field out of range or a
/// hash that does not match, is not a cache.
mod cache {
    use domain::StemRole;

    use super::{Fingerprint, Overview, frames_per_value, value_count};

    const MAGIC: &[u8; 4] = b"OVWF";
    const VERSION: u32 = 1;

    pub(super) fn encode(overview: &Overview, fingerprints: &[Fingerprint]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(64 + 2 * overview.values.len());
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&overview.sample_rate.to_le_bytes());
        bytes.extend_from_slice(&overview.frames_per_value.to_le_bytes());
        bytes.extend_from_slice(&overview.length_frames.to_le_bytes());
        bytes.extend_from_slice(&overview.peak.to_le_bytes());
        bytes.extend_from_slice(&(fingerprints.len() as u32).to_le_bytes());
        for fingerprint in fingerprints {
            bytes.extend_from_slice(&(fingerprint.file.len() as u32).to_le_bytes());
            bytes.extend_from_slice(fingerprint.file.as_bytes());
            bytes.push(match fingerprint.role {
                StemRole::Track => 0,
                StemRole::Click => 1,
                StemRole::Guide => 2,
            });
            bytes.extend_from_slice(&fingerprint.size.to_le_bytes());
            bytes.extend_from_slice(&fingerprint.modified_seconds.to_le_bytes());
            bytes.extend_from_slice(&fingerprint.modified_nanos.to_le_bytes());
        }
        bytes.extend_from_slice(&(overview.values.len() as u32).to_le_bytes());
        for value in &overview.values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        let hash = fnv1a(&bytes);
        bytes.extend_from_slice(&hash.to_le_bytes());
        bytes
    }

    /// The overview and stem fingerprints in `bytes`, or `None` when they
    /// are not a version 1 cache. Never panics, and allocates no more than
    /// the bytes given.
    pub(super) fn decode(bytes: &[u8]) -> Option<(Overview, Vec<Fingerprint>)> {
        let (body, hash) = bytes.split_at_checked(bytes.len().checked_sub(8)?)?;
        if fnv1a(body) != u64::from_le_bytes(hash.try_into().ok()?) {
            return None;
        }
        let mut reader = Reader(body);
        if reader.take(4)? != MAGIC || reader.u32()? != VERSION {
            return None;
        }
        let sample_rate = reader.u32()?;
        let per_value = reader.u32()?;
        let length_frames = reader.u64()?;
        let peak = f32::from_le_bytes(reader.array()?);
        if sample_rate == 0
            || per_value != frames_per_value(sample_rate)
            || !peak.is_finite()
            || peak < 0.0
        {
            return None;
        }
        let stems = reader.u32()?;
        let mut fingerprints = Vec::new();
        for _ in 0..stems {
            let length = usize::try_from(reader.u32()?).ok()?;
            let file = String::from_utf8(reader.take(length)?.to_vec()).ok()?;
            let role = match reader.take(1)?[0] {
                0 => StemRole::Track,
                1 => StemRole::Click,
                2 => StemRole::Guide,
                _ => return None,
            };
            fingerprints.push(Fingerprint {
                file,
                role,
                size: reader.u64()?,
                modified_seconds: reader.u64()?,
                modified_nanos: reader.u32()?,
            });
        }
        let count = reader.u32()?;
        if u64::from(count) != value_count(length_frames, per_value)
            || reader.0.len() != 2 * count as usize
        {
            return None;
        }
        let values = reader
            .0
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let overview = Overview {
            sample_rate,
            frames_per_value: per_value,
            length_frames,
            peak,
            values,
        };
        Some((overview, fingerprints))
    }

    /// The bytes left to decode.
    struct Reader<'a>(&'a [u8]);

    impl<'a> Reader<'a> {
        fn take(&mut self, count: usize) -> Option<&'a [u8]> {
            let (taken, rest) = self.0.split_at_checked(count)?;
            self.0 = rest;
            Some(taken)
        }

        fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
            self.take(N)?.try_into().ok()
        }

        fn u32(&mut self) -> Option<u32> {
            self.array().map(u32::from_le_bytes)
        }

        fn u64(&mut self) -> Option<u64> {
            self.array().map(u64::from_le_bytes)
        }
    }

    /// The 64-bit FNV-1a hash of `bytes`.
    fn fnv1a(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn sample() -> (Overview, Vec<Fingerprint>) {
            let overview = Overview::from_peaks(1_000, 35, &[0.5, 0.25, 0.0, 1.0]);
            let fingerprints = vec![Fingerprint {
                file: "stems/keys.wav".to_owned(),
                role: StemRole::Track,
                size: 1_234,
                modified_seconds: 1_700_000_000,
                modified_nanos: 42,
            }];
            (overview, fingerprints)
        }

        #[test]
        fn a_cache_decodes_to_what_was_encoded() {
            let (overview, fingerprints) = sample();
            let decoded = decode(&encode(&overview, &fingerprints));
            assert_eq!(decoded, Some((overview, fingerprints)));
        }

        #[test]
        fn any_truncation_or_changed_byte_is_not_a_cache() {
            let (overview, fingerprints) = sample();
            let bytes = encode(&overview, &fingerprints);
            for length in 0..bytes.len() {
                assert_eq!(decode(&bytes[..length]), None, "{length} bytes");
            }
            for at in 0..bytes.len() {
                for bit in 0..8 {
                    let mut changed = bytes.clone();
                    changed[at] ^= 1 << bit;
                    assert_eq!(decode(&changed), None, "bit {bit} of byte {at}");
                }
            }
        }

        /// A file with a matching hash whose fields are out of range: the
        /// next version, counts larger than the file, a wrong value count.
        #[test]
        fn hostile_fields_with_a_valid_hash_are_not_a_cache() {
            let (overview, fingerprints) = sample();
            let bytes = encode(&overview, &fingerprints);
            let rehashed = |at: usize, field: &[u8]| {
                let mut changed = bytes[..bytes.len() - 8].to_vec();
                changed[at..at + field.len()].copy_from_slice(field);
                let hash = fnv1a(&changed);
                changed.extend_from_slice(&hash.to_le_bytes());
                changed
            };
            let stems_at = 28;
            let values_at = bytes.len() - 8 - 2 * overview.values.len() - 4;
            for (what, changed) in [
                ("version 2", rehashed(4, &2u32.to_le_bytes())),
                ("no sample rate", rehashed(8, &0u32.to_le_bytes())),
                ("frames per value", rehashed(12, &11u32.to_le_bytes())),
                ("a longer song", rehashed(16, &41u64.to_le_bytes())),
                ("a negative peak", rehashed(24, &(-1.0f32).to_le_bytes())),
                ("a peak not finite", rehashed(24, &f32::NAN.to_le_bytes())),
                (
                    "u32::MAX stems",
                    rehashed(stems_at, &u32::MAX.to_le_bytes()),
                ),
                (
                    "a long file name",
                    rehashed(stems_at + 4, &u32::MAX.to_le_bytes()),
                ),
                (
                    "u32::MAX values",
                    rehashed(values_at, &u32::MAX.to_le_bytes()),
                ),
            ] {
                assert_eq!(decode(&changed), None, "{what}");
            }
        }
    }
}

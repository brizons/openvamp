//! FLAC stems, read with `symphonia`: the STREAMINFO facts, the full decode,
//! and the reads at any frame the disk thread makes.
//!
//! Samples convert exactly as a WAV stem of the same bit depth converts: the
//! decoder gives every sample left-justified in 32 bits, which is shifted
//! back to the file's bit depth and scaled by [`int_scale`].
//!
//! The reader sees only STREAMINFO and the audio frames ([`frames`]), and a
//! seek opens a fresh one: symphonia 0.6.1 keeps the last frame header its
//! parser saw across a seek that ends where its reader already was, then
//! merges the frames after it into one packet, so the stem would lose them.

mod frames;

use std::fs::File;
use std::io;

use domain::Stem;
use engine::StemAudio;
use symphonia::core::audio::{Audio, GenericAudioBufferRef};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as FlacError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::units::Timestamp;
use symphonia::default::codecs::FlacDecoder;
use symphonia::default::formats::FlacReader;

use super::DecodeError;
use crate::{LoadError, SongProblem, int_scale};
use frames::{ENDS_BEFORE_AUDIO, Layout};

/// The audio stops before the length STREAMINFO declares.
const ENDS_EARLY: &str = "Ill-formed FLAC file: the file ends before its declared length";

/// The deepest FLAC the player reads. symphonia 0.6.1 reads a sample in at
/// most 32 bits, but a 32-bit stereo stem's side channel needs 33 and its
/// residuals can need more: it decodes them wrong, or panics in a debug
/// build.
const MAX_BITS: u32 = 24;

/// Samples the full decode reserves up front per byte of file at most, so a
/// STREAMINFO claiming far more frames than the file holds reserves little.
/// Music compresses to more than a byte per four samples; a stem with long
/// silences may hold more, and the decode then grows its buffer.
const RESERVED_SAMPLES_PER_BYTE: u64 = 4;

/// A FLAC stem file, open for decoding, with the facts of its STREAMINFO.
pub(crate) struct FlacStem {
    /// The file, from which every reader reads.
    file: File,
    layout: Layout,
    reader: FlacReader<'static>,
    decoder: FlacDecoder,
    track_id: u32,
    sample_rate: u32,
    channels: u16,
    bits: u32,
    frames: u64,
    /// Size of the file in bytes.
    file_len: u64,
    /// The last decoded packet, interleaved in the file's channels.
    packet: Vec<f32>,
    /// Source frame of the first frame of `packet`.
    packet_start: u64,
    /// Source frame the reader's next packet starts at, when known: after
    /// a seek it is not, until a packet arrives.
    next: Option<u64>,
}

impl FlacStem {
    /// Opens `file`, which starts with the FLAC marker and holds `file_len`
    /// bytes, and reads its metadata up to the first audio frame. A file the
    /// player cannot use is `stem`'s problem: unreadable, without a length,
    /// deeper than 24 bits, or with more than two channels.
    pub(crate) fn open(stem: &Stem, file: File, file_len: u64) -> Result<Self, SongProblem> {
        let unreadable = |reason: String| SongProblem::StemUnreadable {
            id: stem.id.clone(),
            file: stem.file.clone(),
            reason,
        };
        let layout = Layout::read(&file, file_len).map_err(unreadable)?;
        let reader = open_reader(&file, layout).map_err(|error| {
            unreadable(match error {
                FlacError::IoError(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                    ENDS_BEFORE_AUDIO.to_owned()
                }
                other => DecodeError::from(other).to_string(),
            })
        })?;
        let Some((track, Some(CodecParameters::Audio(params)))) = reader
            .tracks()
            .first()
            .map(|track| (track, &track.codec_params))
        else {
            return Err(unreadable(
                "Ill-formed FLAC file: no audio stream".to_owned(),
            ));
        };
        // The reader fills all three from STREAMINFO.
        let (Some(sample_rate), Some(bits), Some(channels)) = (
            params.sample_rate,
            params.bits_per_sample,
            params.channels.as_ref().map(|channels| channels.count()),
        ) else {
            return Err(unreadable(
                "Ill-formed FLAC file: its STREAMINFO is incomplete".to_owned(),
            ));
        };
        if bits > MAX_BITS || !matches!(channels, 1 | 2) {
            let channels = match channels {
                1 => "1 channel".to_owned(),
                n => format!("{n} channels"),
            };
            return Err(SongProblem::UnsupportedFlacFormat {
                id: stem.id.clone(),
                file: stem.file.clone(),
                format: format!("{bits}-bit FLAC, {channels}"),
            });
        }
        let Some(frames) = track.num_frames else {
            return Err(unreadable(
                "Ill-formed FLAC file: its STREAMINFO does not give the length of the audio"
                    .to_owned(),
            ));
        };
        let decoder = FlacDecoder::try_new(params, &AudioDecoderOptions::default())
            .map_err(|error| unreadable(DecodeError::from(error).to_string()))?;
        Ok(Self {
            track_id: track.id,
            file,
            layout,
            reader,
            decoder,
            sample_rate,
            channels: channels as u16,
            bits,
            frames,
            file_len,
            packet: Vec::new(),
            packet_start: 0,
            next: Some(0),
        })
    }

    /// Frames per second.
    pub(crate) fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Length in frames, as STREAMINFO declares it.
    pub(crate) fn frames(&self) -> u64 {
        self.frames
    }

    /// Decodes the whole stem to interleaved `f32`, as [`crate::load_song`]
    /// decodes a WAV stem. Audio past the declared length is left out; audio
    /// that stops before it, or that misses frames, is an error.
    pub(crate) fn decode_all(mut self, stem: &Stem) -> Result<StemAudio, LoadError> {
        let channels = u64::from(self.channels);
        let declared = self.frames.saturating_mul(channels);
        let too_large = |samples| LoadError::StemTooLarge {
            id: stem.id.clone(),
            file: stem.file.clone(),
            samples,
        };
        let decode_error = |source| LoadError::Decode {
            id: stem.id.clone(),
            file: stem.file.clone(),
            source,
        };
        let initial = declared.min(self.file_len.saturating_mul(RESERVED_SAMPLES_PER_BYTE));
        let mut samples = Vec::new();
        reserve(&mut samples, initial).ok_or_else(|| too_large(initial))?;
        let mut done = 0;
        while done < self.frames {
            if !self.holds(done) {
                self.load(done).map_err(decode_error)?;
            }
            let offset = done - self.packet_start;
            let frames = (self.packet_frames() - offset).min(self.frames - done);
            let needed = samples.len() as u64 + frames * channels;
            if needed > samples.capacity() as u64 {
                // Doubles, but never past the declared length.
                let target = needed.max(2 * samples.capacity() as u64).min(declared);
                reserve(&mut samples, target).ok_or_else(|| too_large(target))?;
            }
            let from = (offset * channels) as usize;
            samples.extend_from_slice(&self.packet[from..from + (frames * channels) as usize]);
            done += frames;
        }
        Ok(StemAudio {
            id: stem.id.clone(),
            role: stem.role,
            channels: self.channels,
            samples,
            frames: self.frames,
        })
    }

    /// Fills `out` with the stereo frames from source frame `start`, a mono
    /// stem on both sides; every frame must be inside the stem. When `start`
    /// follows the frames read last, decoding goes on where it stopped;
    /// otherwise it seeks, and the frames of the packet before `start` are
    /// dropped, so the read starts exactly at `start`.
    pub(crate) fn read(&mut self, start: u64, out: &mut [f32]) -> Result<(), DecodeError> {
        let channels = usize::from(self.channels);
        let mut position = start;
        for frame in out.chunks_exact_mut(2) {
            if !self.holds(position) {
                self.load(position)?;
            }
            let at = (position - self.packet_start) as usize * channels;
            frame[0] = self.packet[at];
            frame[1] = self.packet[at + channels - 1];
            position += 1;
        }
        Ok(())
    }

    fn packet_frames(&self) -> u64 {
        (self.packet.len() / usize::from(self.channels)) as u64
    }

    /// Whether the decoded packet holds source frame `position`.
    fn holds(&self, position: u64) -> bool {
        (self.packet_start..self.packet_start + self.packet_frames()).contains(&position)
    }

    /// Decodes the packet holding source frame `position`.
    fn load(&mut self, position: u64) -> Result<(), DecodeError> {
        if self.next != Some(position) {
            self.seek(position)?;
        }
        // An accurate seek lands on the packet holding `position`, or
        // before it when the file is damaged.
        loop {
            self.decode_next(position)?;
            if position < self.packet_start + self.packet_frames() {
                return Ok(());
            }
        }
    }

    /// Moves a fresh reader to the packet holding source frame `position`:
    /// one that has never parsed a frame, so no stale frame header can merge
    /// the frames after `position` into one packet.
    fn seek(&mut self, position: u64) -> Result<(), DecodeError> {
        self.next = None;
        self.packet.clear();
        self.reader = open_reader(&self.file, self.layout).map_err(decode_error)?;
        self.decoder.reset();
        if position == 0 {
            // A fresh reader stands at the first frame.
            self.next = Some(0);
            return Ok(());
        }
        let ts = Timestamp::new(position as i64);
        let to = SeekTo::Timestamp {
            ts,
            track_id: self.track_id,
        };
        self.reader
            .seek(SeekMode::Accurate, to)
            .map_err(decode_error)?;
        Ok(())
    }

    /// Decodes the next packet into `self.packet`, which must start at or
    /// before source frame `wanted`: a packet past it means the frames in
    /// between are missing (a damaged packet the reader skipped), and the
    /// end of the audio before it means the file ends early.
    fn decode_next(&mut self, wanted: u64) -> Result<(), DecodeError> {
        let result = self.decode_packet();
        if result.is_err() {
            self.next = None;
            self.packet.clear();
        }
        let start = result?;
        if start > wanted {
            self.next = None;
            self.packet.clear();
            return Err(DecodeError::Malformed(format!(
                "Ill-formed FLAC file: the audio from frame {wanted} to frame {start} is missing or damaged"
            )));
        }
        Ok(())
    }

    /// Decodes the next packet into `self.packet`; its first source frame.
    fn decode_packet(&mut self) -> Result<u64, DecodeError> {
        let packet = self
            .reader
            .next_packet()
            .map_err(decode_error)?
            .ok_or_else(|| DecodeError::Malformed(ENDS_EARLY.to_owned()))?;
        // FLAC timestamps are 36-bit frame numbers, never negative.
        let start = packet.pts.get() as u64;
        let GenericAudioBufferRef::S32(buffer) = self.decoder.decode(&packet)? else {
            return Err(DecodeError::Malformed(
                "Ill-formed FLAC file: the decoder gave samples of an unexpected format".to_owned(),
            ));
        };
        let channels = usize::from(self.channels);
        let (Some(left), Some(right), true) = (
            buffer.plane(0),
            buffer.plane(channels - 1),
            buffer.num_planes() == channels,
        ) else {
            return Err(DecodeError::Malformed(
                "Ill-formed FLAC file: a frame has another number of channels".to_owned(),
            ));
        };
        let shift = 32 - self.bits;
        let scale = int_scale(self.bits as u16);
        let convert = |sample: i32| (sample >> shift) as f32 * scale;
        self.packet.clear();
        self.packet.reserve(buffer.frames() * channels);
        for (&left, &right) in left.iter().zip(right) {
            self.packet.push(convert(left));
            if channels == 2 {
                self.packet.push(convert(right));
            }
        }
        self.packet_start = start;
        self.next = Some(start + self.packet_frames());
        Ok(start)
    }
}

/// A reader of `file`'s audio frames behind its STREAMINFO, standing at the
/// first frame. It reads from a handle of its own on the same open file, so
/// the file read is always the one checked when the stem was opened.
fn open_reader(file: &File, layout: Layout) -> Result<FlacReader<'static>, FlacError> {
    let source = layout.source(file.try_clone()?);
    let stream = MediaSourceStream::new(Box::new(source), MediaSourceStreamOptions::default());
    FlacReader::try_new(stream, FormatOptions::default())
}

/// A reader error as a decode error: the end of the file before the frames
/// wanted means the file ends early.
fn decode_error(error: FlacError) -> DecodeError {
    match error {
        FlacError::IoError(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            DecodeError::Malformed(ENDS_EARLY.to_owned())
        }
        other => other.into(),
    }
}

/// Grows `samples` to hold `capacity` samples in all; `None` when that does
/// not fit in memory.
fn reserve(samples: &mut Vec<f32>, capacity: u64) -> Option<()> {
    let additional = usize::try_from(capacity).ok()?.checked_sub(samples.len())?;
    samples.try_reserve_exact(additional).ok()
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use domain::StemRole;
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;

    use super::*;

    /// Measures how long a read at a new position takes (a seek, then the
    /// packet holding it) in a stereo 48 kHz 24-bit FLAC that flacenc wrote,
    /// without a SEEKTABLE. `cargo test -p library --lib seek_timing --
    /// --ignored --nocapture`; `SEEK_TIMING_MINUTES` sets the length
    /// (default 5) and `SEEK_TIMING_FLAC` reads that file instead.
    #[test]
    #[ignore = "a measurement, not a check"]
    fn seek_timing() {
        let dir = tempfile::tempdir().unwrap();
        let path = std::env::var_os("SEEK_TIMING_FLAC").map_or_else(
            || {
                let minutes: usize = std::env::var("SEEK_TIMING_MINUTES")
                    .map_or(5, |minutes| minutes.parse().unwrap());
                let path = dir.path().join("long.flac");
                let started = Instant::now();
                std::fs::write(&path, music_like(minutes * 60 * 48_000)).unwrap();
                eprintln!("encoded {minutes} min in {:?}", started.elapsed());
                path
            },
            std::path::PathBuf::from,
        );
        let stem = Stem {
            id: "long".to_owned(),
            file: "long.flac".to_owned(),
            part: "long".to_owned(),
            role: StemRole::Track,
        };
        let open = || {
            let file = File::open(&path).unwrap();
            let len = file.metadata().unwrap().len();
            FlacStem::open(&stem, file, len).unwrap()
        };
        let mut flac = open();
        let frames = flac.frames();
        eprintln!(
            "{} bytes, {frames} frames",
            std::fs::metadata(&path).unwrap().len()
        );
        let mut out = vec![0.0; 2 * 2_048];
        for at in [
            frames - 1,
            48_000,
            frames / 2 + 1_234,
            60 * 48_000 + 17,
            frames - 48_000,
            5,
        ]
        .into_iter()
        .filter(|&at| at < frames)
        {
            let started = Instant::now();
            flac.read(at, &mut out[..2]).unwrap();
            let seek = started.elapsed();
            let next = (frames - at - 1).min(2_048) as usize;
            let started = Instant::now();
            flac.read(at + 1, &mut out[..2 * next]).unwrap();
            eprintln!(
                "frame {at:>9}: seek and first frame {seek:>10.2?}; next {next:>5} frames {:>10.2?}",
                started.elapsed()
            );
        }
    }

    /// Stereo 24-bit audio that compresses about as music does: two tones and
    /// a little noise, as FLAC bytes.
    fn music_like(frames: usize) -> Vec<u8> {
        let mut state = 1u64;
        let mut samples = Vec::with_capacity(2 * frames);
        for frame in 0..frames {
            let t = frame as f64 / 48_000.0;
            for (channel, hz) in [(0.0, 220.0), (1.0, 330.0)] {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let noise = ((state >> 40) as i32 - (1 << 23)) / 64;
                let tone = (t * hz * std::f64::consts::TAU + channel).sin() * 3_000_000.0;
                samples.push(tone as i32 + noise);
            }
        }
        let config = flacenc::config::Encoder::default().into_verified().unwrap();
        let source = flacenc::source::MemSource::from_samples(&samples, 2, 24, 48_000);
        let mut stream = flacenc::encode_with_fixed_block_size(&config, source, 4_096).unwrap();
        stream
            .stream_info_mut()
            .set_block_sizes(4_096, 4_096)
            .unwrap();
        let mut sink = flacenc::bitsink::ByteSink::new();
        stream.write(&mut sink).unwrap();
        sink.as_slice().to_vec()
    }
}

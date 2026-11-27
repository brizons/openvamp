//! The audio the player cannot read as it is: its facts, read with
//! `symphonia` without decoding more than one packet, and its transcoding to
//! 24-bit FLAC with `flacenc`, written as it is decoded so memory stays flat.

use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use flacenc::bitsink::ByteSink;
use flacenc::component::{BitRepr, Stream, StreamInfo};
use flacenc::config::Encoder;
use flacenc::error::{Verified, Verify};
use flacenc::source::{Context, Fill, FrameBuf};
use symphonia::core::codecs::audio::well_known::CODEC_ID_FLAC;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Timestamp;

use super::SourceFormat;
use super::mp4;
use crate::open_regular_file;

/// Bits per sample of the FLAC files the import writes.
const FLAC_BITS: usize = 24;

/// Frames per FLAC block: the encoder's default.
const FLAC_BLOCK: usize = 4_096;

/// The deepest FLAC the import decodes, as the player reads it: `symphonia`
/// 0.6.1 decodes deeper FLAC wrong (a 32-bit stereo side channel needs 33
/// bits), so such a file is refused rather than transcoded.
const MAX_FLAC_BITS: u32 = 24;

/// Largest 24-bit sample, as a float.
const MAX_24: f64 = 8_388_607.0;

/// Smallest 24-bit sample, as a float.
const MIN_24: f64 = -8_388_608.0;

/// Frames between two progress reports while transcoding: about a second.
const REPORT_EVERY: u64 = 48_000;

/// What the import reads from a file it transcodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AudioFacts {
    /// Frames per second.
    pub(super) sample_rate: u32,
    /// Channels.
    pub(super) channels: u16,
    /// Frames the stem holds once delay and padding are trimmed.
    pub(super) frames: u64,
    /// Decoded frames to drop first: the encoder delay an M4A's edit list
    /// or `iTunSMPB` tag records (an MP3's decoder drops its own).
    pub(super) skip: u64,
    /// Whether `frames` is only an estimate (an MP3 without a Xing or LAME
    /// header, an M4A that records no delay): the stem is then as long as
    /// its decoder makes it.
    pub(super) estimated: bool,
}

/// A file open for decoding: its first audio track and a gapless decoder.
struct Decoding {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    /// Frames the container declares, without delay and padding, if any.
    declared: Option<u64>,
    /// Whether the container records the encoder delay or padding, which
    /// the decoder then trims.
    gapless: bool,
}

/// Why a file could not be transcoded.
#[derive(Debug)]
pub(super) enum TranscodeError {
    /// The stop flag was raised.
    Stopped,
    /// The source could not be read or decoded, or does not match its probe.
    Source(String),
    /// The FLAC file could not be written.
    Write(io::Error),
}

impl From<io::Error> for TranscodeError {
    fn from(error: io::Error) -> Self {
        Self::Write(error)
    }
}

/// Opens the regular file at `path` (a FIFO is refused without being
/// opened) as `format` (the extension is only a hint: the content decides),
/// with the decoder of its first audio track.
fn open(path: &Path, format: SourceFormat) -> Result<Decoding, String> {
    let (file, _) = open_regular_file(path).map_err(|error| error.to_string())?;
    let source = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    hint.with_extension(match format {
        SourceFormat::Wav => "wav",
        SourceFormat::Flac => "flac",
        SourceFormat::Mp3 => "mp3",
        SourceFormat::M4a => "m4a",
    });
    let reader = symphonia::default::get_probe()
        .probe(
            &hint,
            source,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|error| describe(&error))?;
    let track = reader
        .default_track(TrackType::Audio)
        .ok_or_else(|| "it has no audio".to_owned())?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|params| params.audio())
        .ok_or_else(|| "it has no audio".to_owned())?;
    if let Some(bits) = params
        .bits_per_sample
        .filter(|&bits| params.codec == CODEC_ID_FLAC && bits > MAX_FLAC_BITS)
    {
        return Err(format!(
            "it is {bits}-bit FLAC, and FLAC is decoded up to {MAX_FLAC_BITS}-bit"
        ));
    }
    let mut options = AudioDecoderOptions::default();
    options.gapless = true;
    let decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &options)
        .map_err(|error| describe(&error))?;
    let gapless = track.delay.is_some() || track.padding.is_some();
    let (track_id, declared) = (track.id, track.num_frames);
    Ok(Decoding {
        reader,
        decoder,
        track_id,
        declared,
        gapless,
    })
}

/// A decoder error in words.
fn describe(error: &SymphoniaError) -> String {
    match error {
        SymphoniaError::Unsupported(what) => format!("unsupported {what}"),
        other => other.to_string(),
    }
}

/// Reads the facts of `path` as `format`: the rate and the channels of its
/// first decoded packet, and its length. The length is the one the
/// container declares for WAV and FLAC (whose last declared frame must then
/// decode: a file cut short is refused), and for an MP3 whose header records
/// its delay and padding (the decoder trims them; `symphonia` 0.6.1 leaves
/// them in the packets' durations). Otherwise the durations of every packet
/// are added up without decoding them, and an MP3's or M4A's length is only
/// an estimate: an MP3 decoder can give a packet less.
pub(super) fn probe(path: &Path, format: SourceFormat) -> Result<AudioFacts, ProbeError> {
    let mut decoding = open(path, format).map_err(ProbeError::Undecodable)?;
    let trusted = match format {
        SourceFormat::Wav | SourceFormat::Flac => decoding.declared,
        SourceFormat::Mp3 => decoding.declared.filter(|_| decoding.gapless),
        SourceFormat::M4a => None,
    };
    let mut total = 0;
    let mut spec = None;
    while let Some(packet) =
        next_packet(decoding.reader.as_mut()).map_err(ProbeError::Undecodable)?
    {
        if packet.track_id != decoding.track_id {
            continue;
        }
        total += packet.dur.get();
        if spec.is_none() {
            let decoded = decoding
                .decoder
                .decode(&packet)
                .map_err(|error| ProbeError::Undecodable(describe(&error)))?;
            spec = Some((
                decoded.spec().rate(),
                decoded.spec().channels().count() as u16,
            ));
            if let Some(declared) = trusted {
                total = declared;
                break;
            }
        }
    }
    let (sample_rate, channels) =
        spec.ok_or_else(|| ProbeError::Undecodable("it holds no audio".to_owned()))?;
    if matches!(format, SourceFormat::Wav | SourceFormat::Flac)
        && let Some(declared) = trusted
        && !reaches(&mut decoding, declared)
    {
        return Err(ProbeError::CutShort(declared));
    }
    let (skip, frames, estimated) = match format {
        SourceFormat::M4a => {
            let edits = File::open(path)
                .and_then(|mut file| mp4::edit_list(&mut file))
                .map_err(|error| ProbeError::Undecodable(error.to_string()))?;
            match edits {
                Some(edits) => {
                    let after = total.saturating_sub(edits.skip);
                    let frames = edits.keep.map_or(after, |keep| keep.min(after));
                    (edits.skip, frames, false)
                }
                None => (0, total, true),
            }
        }
        SourceFormat::Mp3 => (0, total, trusted.is_none()),
        _ => (0, total, false),
    };
    Ok(AudioFacts {
        sample_rate,
        channels,
        frames,
        skip,
        estimated,
    })
}

/// Why [`probe`] found no facts.
#[derive(Debug)]
pub(super) enum ProbeError {
    /// Nothing decodes the file; what the decoder said.
    Undecodable(String),
    /// A WAV or FLAC file whose audio stops before the frames its header
    /// declares, which it holds.
    CutShort(u64),
}

/// Why a WAV or FLAC file whose audio stops before its header says is
/// refused.
pub(super) fn cut_short(declared: u64) -> String {
    format!("it ends before the {declared} frames its header declares")
}

/// Whether the audio of `decoding` reaches the `declared` frames: the packet
/// holding the last one decodes.
fn reaches(decoding: &mut Decoding, declared: u64) -> bool {
    let Some(last) = declared
        .checked_sub(1)
        .and_then(|last| i64::try_from(last).ok())
    else {
        return false;
    };
    let to = SeekTo::Timestamp {
        ts: Timestamp::new(last),
        track_id: decoding.track_id,
    };
    if decoding.reader.seek(SeekMode::Accurate, to).is_err() {
        return false;
    }
    decoding.decoder.reset();
    // An accurate seek lands on the packet holding the frame, or before it.
    loop {
        let Ok(Some(packet)) = next_packet(decoding.reader.as_mut()) else {
            return false;
        };
        if packet.track_id != decoding.track_id {
            continue;
        }
        let Ok(decoded) = decoding.decoder.decode(&packet) else {
            return false;
        };
        let end = packet.pts.get().saturating_add(decoded.frames() as i64);
        if end > last {
            return true;
        }
    }
}

/// The next packet, `None` at the end of the audio.
fn next_packet(
    reader: &mut dyn FormatReader,
) -> Result<Option<symphonia::core::packet::Packet>, String> {
    match reader.next_packet() {
        Ok(packet) => Ok(packet),
        // Some formats end with an unexpected end of file.
        Err(SymphoniaError::IoError(error)) if error.kind() == io::ErrorKind::UnexpectedEof => {
            Ok(None)
        }
        Err(error) => Err(describe(&error)),
    }
}

/// Decodes `path` as `format` and writes it to `out` as 24-bit FLAC: the
/// `facts.frames` frames after the first `facts.skip`, or every frame after
/// them when `facts.frames` is an estimate. Returns the frames written.
/// `report` gets the frames written so far about every second of audio and
/// at the end; when it returns `false`, the transcoding stops. The file at
/// `out` is left for the caller to remove on error.
pub(super) fn transcode(
    path: &Path,
    format: SourceFormat,
    facts: AudioFacts,
    out: &Path,
    report: &mut dyn FnMut(u64) -> bool,
) -> Result<u64, TranscodeError> {
    let mut decoding = open(path, format).map_err(TranscodeError::Source)?;
    let mut writer = FlacWriter::create(out, facts.sample_rate, facts.channels)?;
    let channels = usize::from(facts.channels);
    let mut decoded = Vec::<f64>::new();
    let mut samples = Vec::<i32>::new();
    let mut to_skip = facts.skip;
    let mut written = 0;
    let mut reported = 0;
    while facts.estimated || written < facts.frames {
        let Some(packet) = next_packet(decoding.reader.as_mut()).map_err(TranscodeError::Source)?
        else {
            break;
        };
        if packet.track_id != decoding.track_id {
            continue;
        }
        let buffer = decoding.decoder.decode(&packet).map_err(|error| {
            TranscodeError::Source(format!(
                "the audio cannot be decoded after frame {written}: {}",
                describe(&error)
            ))
        })?;
        if buffer.spec().rate() != facts.sample_rate || buffer.spec().channels().count() != channels
        {
            return Err(TranscodeError::Source(format!(
                "the audio changes format after frame {written}"
            )));
        }
        buffer.copy_to_vec_interleaved(&mut decoded);
        let mut frames = &decoded[..];
        let skipped = to_skip.min((frames.len() / channels) as u64);
        frames = &frames[skipped as usize * channels..];
        to_skip -= skipped;
        let available = (frames.len() / channels) as u64;
        let wanted = if facts.estimated {
            available
        } else {
            (facts.frames - written).min(available)
        };
        frames = &frames[..wanted as usize * channels];
        samples.clear();
        samples.extend(frames.iter().map(|&sample| to_24_bits(sample)));
        writer.write(&samples)?;
        written += wanted;
        if written - reported >= REPORT_EVERY {
            reported = written;
            if !report(written) {
                return Err(TranscodeError::Stopped);
            }
        }
    }
    if written == 0 || (!facts.estimated && written < facts.frames) {
        return Err(TranscodeError::Source(format!(
            "the audio ends after {written} frames, before the {} the file declares",
            facts.frames
        )));
    }
    writer.finish()?;
    if !report(written) {
        return Err(TranscodeError::Stopped);
    }
    Ok(written)
}

/// A sample from -1 to 1 as a 24-bit integer, rounded to the nearest and
/// clamped: exact for a source of 24 bits or fewer.
fn to_24_bits(sample: f64) -> i32 {
    (sample * 8_388_608.0).round().clamp(MIN_24, MAX_24) as i32
}

/// A FLAC file written block by block: a placeholder STREAMINFO first, the
/// frames as they are encoded, then the real STREAMINFO over the
/// placeholder.
///
/// No frame is larger than the verbatim frame of its samples. flacenc 0.5.1
/// picks each channel's coding from an entropy estimate, and its Rice codes
/// stop at parameter 14, so for loud noise it can pick a FIXED predictor
/// whose residuals take far more bits than the samples (frames of tens of
/// MB, whose size then wraps STREAMINFO's 24-bit frame-size fields). A frame
/// that comes out larger than its raw samples is therefore encoded again as
/// verbatim, and the smaller of the two is written; STREAMINFO's frame
/// sizes are those of the frames written.
struct FlacWriter {
    file: BufWriter<File>,
    config: Verified<Encoder>,
    /// Verbatim subframes, channels coded independently.
    verbatim: Verified<Encoder>,
    info: StreamInfo,
    framebuf: FrameBuf,
    context: Context,
    channels: usize,
    /// Interleaved samples not yet encoded: less than one block.
    pending: Vec<i32>,
    frame_number: usize,
    sink: ByteSink,
}

impl FlacWriter {
    fn create(path: &Path, sample_rate: u32, channels: u16) -> io::Result<Self> {
        let invalid = |error: flacenc::error::VerifyError| {
            io::Error::new(io::ErrorKind::InvalidInput, error.to_string())
        };
        let channels = usize::from(channels);
        let info = StreamInfo::new(sample_rate as usize, channels, FLAC_BITS).map_err(invalid)?;
        let config = Encoder::default()
            .into_verified()
            .map_err(|(_, error)| invalid(error))?;
        let mut verbatim = Encoder::default();
        verbatim.subframe_coding.use_constant = false;
        verbatim.subframe_coding.use_fixed = false;
        verbatim.subframe_coding.use_lpc = false;
        verbatim.stereo_coding.use_leftside = false;
        verbatim.stereo_coding.use_rightside = false;
        verbatim.stereo_coding.use_midside = false;
        let verbatim = verbatim
            .into_verified()
            .map_err(|(_, error)| invalid(error))?;
        let mut writer = Self {
            file: BufWriter::new(File::create(path)?),
            config,
            verbatim,
            framebuf: FrameBuf::with_size(channels, FLAC_BLOCK).map_err(invalid)?,
            context: Context::new(FLAC_BITS, channels),
            info,
            channels,
            pending: Vec::with_capacity(FLAC_BLOCK * channels),
            frame_number: 0,
            sink: ByteSink::new(),
        };
        let header = writer.header()?;
        writer.file.write_all(&header)?;
        Ok(writer)
    }

    /// `fLaC` and the STREAMINFO block as they stand.
    fn header(&self) -> io::Result<Vec<u8>> {
        let mut sink = ByteSink::new();
        Stream::with_stream_info(self.info.clone())
            .write(&mut sink)
            .map_err(|error| io::Error::other(error.to_string()))?;
        Ok(sink.as_slice().to_vec())
    }

    /// Adds interleaved 24-bit samples.
    fn write(&mut self, mut samples: &[i32]) -> io::Result<()> {
        let block = FLAC_BLOCK * self.channels;
        while !samples.is_empty() {
            let take = (block - self.pending.len()).min(samples.len());
            self.pending.extend_from_slice(&samples[..take]);
            samples = &samples[take..];
            if self.pending.len() == block {
                self.encode_pending()?;
            }
        }
        Ok(())
    }

    fn encode_pending(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let fill_error = |error: flacenc::error::SourceError| io::Error::other(error.to_string());
        self.framebuf
            .fill_interleaved(&self.pending)
            .map_err(fill_error)?;
        self.context
            .fill_interleaved(&self.pending)
            .map_err(fill_error)?;
        let raw_bits = self.pending.len() * FLAC_BITS;
        self.pending.clear();
        let encode = |config: &Verified<Encoder>| {
            flacenc::encode_fixed_size_frame(config, &self.framebuf, self.frame_number, &self.info)
                .map_err(|error| io::Error::other(error.to_string()))
        };
        let mut frame = encode(&self.config)?;
        if frame.count_bits() > raw_bits {
            let verbatim = encode(&self.verbatim)?;
            if verbatim.count_bits() < frame.count_bits() {
                frame = verbatim;
            }
        }
        self.frame_number += 1;
        self.info.update_frame_info(&frame);
        self.sink.clear();
        frame
            .write(&mut self.sink)
            .map_err(|error| io::Error::other(error.to_string()))?;
        self.file.write_all(self.sink.as_slice())
    }

    /// Encodes the last, shorter block, writes the final STREAMINFO and
    /// flushes the file to disk.
    fn finish(mut self) -> io::Result<()> {
        self.encode_pending()?;
        // flacenc records the short last block as the minimum block size,
        // which makes the stream read as variable-size blocks numbered as
        // fixed-size ones; decoders refuse that. The FLAC format leaves the
        // last block out of the minimum.
        self.info
            .set_block_sizes(FLAC_BLOCK, FLAC_BLOCK)
            .map_err(|error| io::Error::other(error.to_string()))?;
        self.info.set_md5_digest(&self.context.md5_digest());
        let header = self.header()?;
        let mut file = self.file.into_inner().map_err(|error| error.into_error())?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header)?;
        file.sync_all()
    }
}

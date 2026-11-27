//! The disk thread: decodes the blocks the sources ask for.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use domain::Stem;
use hound::SampleFormat;
use rtrb::{Consumer, Producer};

use super::{Block, Counters, Gate};
use crate::decode::FlacStem;
use crate::{DecodeError, LoadError, OpenedStem, StemReader, int_scale};

/// How long the disk thread sleeps when it has nothing to do. The audio
/// thread never wakes it (that would be a system call), so this bounds how
/// late a new request is noticed.
const IDLE_SLEEP: Duration = Duration::from_millis(1);

/// Reads blocks of one WAV or FLAC stem as stereo `f32`.
pub(super) struct Decoder {
    file: StemFile,
    frames: u64,
}

/// The open file of a [`Decoder`].
enum StemFile {
    Wav {
        reader: StemReader,
        channels: usize,
        /// `None` for float samples.
        int_scale: Option<f32>,
    },
    Flac(Box<FlacStem>),
}

impl Decoder {
    /// Rejects a file whose header declares more samples than it holds, as
    /// [`crate::load_song`] does when it decodes it. A FLAC file must decode
    /// its last declared frame.
    pub(super) fn new(stem: &Stem, opened: OpenedStem) -> Result<Self, LoadError> {
        let decode_error = |source| LoadError::Decode {
            id: stem.id.clone(),
            file: stem.file.clone(),
            source,
        };
        match opened {
            OpenedStem::Wav {
                reader,
                max_samples,
            } => {
                if u64::from(reader.len()) > max_samples {
                    return Err(decode_error(
                        hound::Error::FormatError(
                            "the file holds fewer samples than its header declares",
                        )
                        .into(),
                    ));
                }
                let spec = reader.spec();
                Ok(Self {
                    frames: u64::from(reader.duration()),
                    file: StemFile::Wav {
                        channels: usize::from(spec.channels),
                        int_scale: (spec.sample_format == SampleFormat::Int)
                            .then(|| int_scale(spec.bits_per_sample)),
                        reader,
                    },
                })
            }
            OpenedStem::Flac(mut flac) => {
                let frames = flac.frames();
                // STREAMINFO never declares 0 frames: a stem without a
                // length is refused when it is opened.
                flac.read(frames - 1, &mut [0.0; 2]).map_err(decode_error)?;
                Ok(Self {
                    frames,
                    file: StemFile::Flac(flac),
                })
            }
        }
    }

    pub(super) fn frames(&self) -> u64 {
        self.frames
    }

    /// Fills `out` with the stereo frames from source frame `start`: a mono
    /// stem on both sides, silence past the end. Samples convert exactly as
    /// [`crate::load_song`] converts them.
    fn decode(&mut self, start: u64, out: &mut [f32]) -> Result<(), DecodeError> {
        out.fill(0.0);
        if start >= self.frames {
            return Ok(());
        }
        let frames = ((out.len() / 2) as u64).min(self.frames - start) as usize;
        match &mut self.file {
            StemFile::Wav {
                reader,
                channels,
                int_scale,
            } => decode_wav(reader, *channels, *int_scale, start, &mut out[..2 * frames])
                .map_err(DecodeError::from),
            StemFile::Flac(flac) => flac.read(start, &mut out[..2 * frames]),
        }
    }
}

/// Fills `out` with the stereo frames of a WAV stem from source frame
/// `start`, all inside the stem.
fn decode_wav(
    reader: &mut StemReader,
    channels: usize,
    int_scale: Option<f32>,
    start: u64,
    out: &mut [f32],
) -> hound::Result<()> {
    let frames = out.len() / 2;
    let start = u32::try_from(start).map_err(|_| hound::Error::TooWide)?;
    reader.seek(start)?;
    match int_scale {
        Some(scale) => {
            let samples = reader
                .samples::<i32>()
                .map(|sample| sample.map(|sample| sample as f32 * scale));
            fill(out, channels, frames, samples)
        }
        None => fill(out, channels, frames, reader.samples::<f32>()),
    }
}

fn fill(
    out: &mut [f32],
    channels: usize,
    frames: usize,
    mut samples: impl Iterator<Item = hound::Result<f32>>,
) -> hound::Result<()> {
    let mut next = || {
        samples.next().unwrap_or(Err(hound::Error::FormatError(
            "the file ends before its declared length",
        )))
    };
    for frame in out.chunks_exact_mut(2).take(frames) {
        let left = next()?;
        let right = if channels == 1 { left } else { next()? };
        frame[0] = left;
        frame[1] = right;
    }
    Ok(())
}

/// The disk side of one stem.
pub(super) struct DiskStem {
    id: String,
    decoder: Decoder,
    block_frames: u64,
    requests: Consumer<u64>,
    filled: Producer<Block>,
    returns: Consumer<Block>,
    /// Blocks no one holds.
    free: Vec<Block>,
    /// Requests not served yet.
    backlog: VecDeque<u64>,
}

impl DiskStem {
    pub(super) fn new(
        id: String,
        decoder: Decoder,
        block_frames: usize,
        pool: usize,
        (requests, filled, returns): (Consumer<u64>, Producer<Block>, Consumer<Block>),
    ) -> Self {
        let free = (0..pool)
            .map(|_| Block {
                index: 0,
                ok: false,
                data: vec![0.0; 2 * block_frames].into_boxed_slice(),
            })
            .collect();
        Self {
            id,
            decoder,
            block_frames: block_frames as u64,
            requests,
            filled,
            returns,
            free,
            backlog: VecDeque::with_capacity(pool),
        }
    }

    /// Takes back returned blocks and new requests.
    fn collect(&mut self) {
        while let Ok(block) = self.returns.pop() {
            self.free.push(block);
        }
        while let Ok(index) = self.requests.pop() {
            self.backlog.push_back(index);
        }
    }

    fn frames_of(&self, index: u64) -> std::ops::Range<u64> {
        let start = index * self.block_frames;
        start..start + self.block_frames
    }

    fn allowed(&self, gate: Option<&Gate>, index: u64) -> bool {
        gate.is_none_or(|gate| gate(&self.id, self.frames_of(index)))
    }

    /// Decodes and sends the oldest request the gate allows; whether one was
    /// sent.
    fn serve_one(&mut self, gate: Option<&Gate>, counters: &Counters) -> bool {
        let Some(position) =
            (0..self.backlog.len()).find(|&position| self.allowed(gate, self.backlog[position]))
        else {
            return false;
        };
        let Some(mut block) = self.free.pop() else {
            return false;
        };
        let index = self.backlog[position];
        block.index = index;
        block.ok = self
            .decoder
            .decode(index * self.block_frames, &mut block.data)
            .is_ok();
        if !block.ok {
            counters.errors.fetch_add(1, Ordering::SeqCst);
        }
        match self.filled.push(block) {
            Ok(()) => {
                self.backlog.remove(position);
                counters.completed.fetch_add(1, Ordering::SeqCst);
                true
            }
            Err(rtrb::PushError::Full(block)) => {
                self.free.push(block);
                false
            }
        }
    }

    fn held(&self, gate: Option<&Gate>) -> u64 {
        self.backlog
            .iter()
            .filter(|&&index| !self.allowed(gate, index))
            .count() as u64
    }
}

/// Clears `running` when the disk thread ends, panics included.
struct Running<'a>(&'a Counters);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::SeqCst);
    }
}

/// Serves the stems round-robin, one block per stem per round, so a stem
/// with many requests does not hold the others back, until the sources are
/// dropped.
pub(super) fn run(mut stems: Vec<DiskStem>, counters: &Counters, gate: Option<&Gate>) {
    let _running = Running(counters);
    while !counters.stop.load(Ordering::SeqCst) {
        counters.passes_started.fetch_add(1, Ordering::SeqCst);
        for stem in &mut stems {
            stem.collect();
        }
        let mut served = false;
        loop {
            let mut any = false;
            for stem in &mut stems {
                any |= stem.serve_one(gate, counters);
            }
            if !any {
                break;
            }
            served = true;
        }
        let held = stems.iter().map(|stem| stem.held(gate)).sum();
        counters.held.store(held, Ordering::SeqCst);
        counters.passes_finished.fetch_add(1, Ordering::SeqCst);
        if !served {
            thread::sleep(IDLE_SLEEP);
        }
    }
}

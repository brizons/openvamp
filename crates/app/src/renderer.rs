//! The audio-thread side of a session.
//!
//! The [`Deck`] (the current item's player, and the next one when a setlist
//! cues it) and the writing end of the state snapshot travel together as
//! one [`AudioState`]. While a device plays, an [`AudioRenderer`] owns it
//! on the audio thread; when the renderer is dropped (the stream closed or
//! the device was lost), it hands the state back to the session through a
//! one-slot `rtrb` queue instead of dropping it. The players are therefore
//! only ever dropped on the control thread: dropping one drops its last
//! streaming source, which joins the disk thread (advisory A6). The deck
//! sends the players it no longer needs back the same way.

use std::fmt;

use engine::live::{Deck, DeckStatus, Status};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::output::MAX_BUFFER_FRAMES;

/// What the audio thread publishes after every buffer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Report {
    /// The deck's snapshot.
    pub(crate) deck: DeckStatus,
    /// Whether every source of the current item can start playing from the
    /// playhead; always true while the transport runs.
    pub(crate) ready: bool,
    /// Messages the deck had applied: the report describes the state every
    /// message sent up to this count left.
    pub(crate) applied: u64,
}

impl Report {
    /// The current item's player.
    pub(crate) fn status(&self) -> &Status {
        &self.deck.current
    }
}

/// The deck and the writing end of its snapshot.
pub(crate) struct AudioState {
    pub(crate) deck: Deck,
    pub(crate) reports: triple_buffer::Input<Report>,
    /// Channels the deck's players render.
    channels: usize,
    /// Channels the stream takes, at most `channels`: the first ones the
    /// players render. Set before each stream opens.
    out_channels: usize,
    /// Frames of `channels` channels, for a stream that takes fewer.
    scratch: Vec<f32>,
}

impl AudioState {
    /// The state of `deck`, whose players render `channels` channels, for a
    /// stream that takes them all until [`Self::set_out_channels`] says
    /// otherwise.
    pub(crate) fn new(deck: Deck, reports: triple_buffer::Input<Report>, channels: usize) -> Self {
        Self {
            deck,
            reports,
            channels,
            out_channels: channels,
            scratch: vec![0.0; channels * MAX_BUFFER_FRAMES as usize],
        }
    }

    /// Channels the deck's players render.
    pub(crate) fn channels(&self) -> usize {
        self.channels
    }

    /// The stream to open takes the first `channels` channels.
    pub(crate) fn set_out_channels(&mut self, channels: usize) {
        self.out_channels = channels.clamp(1, self.channels);
    }

    /// Renders `out`, frames of the stream's channels, and publishes the
    /// result. Real-time safe.
    pub(crate) fn render(&mut self, out: &mut [f32]) {
        if self.out_channels == self.channels {
            self.deck.process(out);
        } else {
            self.render_first(out);
        }
        self.publish();
    }

    /// Renders the players' channels into the scratch buffer and keeps the
    /// first ones the stream takes; a trailing partial frame is silent.
    fn render_first(&mut self, out: &mut [f32]) {
        let (channels, taken) = (self.channels, self.out_channels);
        let piece = self.scratch.len() / channels;
        let whole = out.len() - out.len() % taken;
        let (frames, rest) = out.split_at_mut(whole);
        rest.fill(0.0);
        for chunk in frames.chunks_mut(piece * taken) {
            let rendered = &mut self.scratch[..channels * (chunk.len() / taken)];
            self.deck.process(rendered);
            for (frame, heard) in chunk
                .chunks_exact_mut(taken)
                .zip(rendered.chunks_exact(channels))
            {
                frame.copy_from_slice(&heard[..taken]);
            }
        }
    }

    /// Publishes the deck's state. Real-time safe: `Report` is plain data,
    /// and the triple buffer neither allocates nor locks.
    pub(crate) fn publish(&mut self) {
        let deck = self.deck.status();
        let ready = deck.current.transport.is_running() || self.deck.playhead_ready();
        let applied = self.deck.applied_messages();
        self.reports.write(Report {
            deck,
            ready,
            applied,
        });
    }
}

/// Renders a session's audio. An [`crate::AudioOutput`] gets one with every
/// stream it opens and calls [`Self::render`] from its audio thread.
pub struct AudioRenderer {
    state: Option<Box<AudioState>>,
    home: Producer<Box<AudioState>>,
}

impl AudioRenderer {
    /// A renderer for `state`, and the end the session takes it back from.
    pub(crate) fn new(state: Box<AudioState>) -> (Self, Consumer<Box<AudioState>>) {
        let (home, back) = RingBuffer::new(1);
        let renderer = Self {
            state: Some(state),
            home,
        };
        (renderer, back)
    }

    /// Fills `out` with interleaved frames of the channels the stream was
    /// requested with ([`crate::OutputRequest::channels`]; a trailing partial
    /// frame is left at 0.0) and publishes the session's state. Real-time
    /// safe: no allocation, no lock, no I/O.
    pub fn render(&mut self, out: &mut [f32]) {
        match self.state.as_mut() {
            Some(state) => state.render(out),
            None => out.fill(0.0),
        }
    }
}

impl Drop for AudioRenderer {
    /// Hands the deck back to its session rather than dropping it here,
    /// on what may be the audio thread. Pushing a box moves a pointer into a
    /// slot that already exists: nothing is allocated or freed.
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            // The slot holds the only state there is, so it is free.
            let _ = self.home.push(state);
        }
    }
}

impl fmt::Debug for AudioRenderer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioRenderer").finish_non_exhaustive()
    }
}

//! Per-sample linear gain ramps.

/// A gain moving linearly from one level to another, one step per frame.
///
/// The value of each frame depends only on how many frames the ramp has
/// produced, never on how the frames were split into buffers, and the last
/// frame of a ramp is exactly its target.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Ramp {
    from: f32,
    to: f32,
    len: u64,
    pos: u64,
}

impl Ramp {
    /// A ramp resting at `value`.
    pub(crate) fn at(value: f32) -> Self {
        Self {
            from: value,
            to: value,
            len: 0,
            pos: 0,
        }
    }

    /// The level reached: the gain of the most recent frame, or the start
    /// level before the first frame.
    pub(crate) fn level(&self) -> f32 {
        self.value(self.pos)
    }

    /// Whether the ramp has reached its target.
    pub(crate) fn is_done(&self) -> bool {
        self.pos >= self.len
    }

    /// Frames left before the ramp reaches its target.
    pub(crate) fn remaining(&self) -> u64 {
        self.len.saturating_sub(self.pos)
    }

    /// Gain of the next frame.
    pub(crate) fn next(&mut self) -> f32 {
        if self.pos < self.len {
            self.pos += 1;
        }
        self.value(self.pos)
    }

    /// Starts moving from the current level to `to` at a constant rate: a
    /// full swing from 0 to 1 takes `full_scale` frames, a shorter one
    /// proportionally fewer (at least one frame when the level changes).
    pub(crate) fn retarget(&mut self, to: f32, full_scale: u64) {
        let from = self.level();
        let len = if from == to {
            0
        } else {
            let frames = (f64::from((to - from).abs()) * full_scale as f64).round() as u64;
            frames.max(1)
        };
        *self = Self {
            from,
            to,
            len,
            pos: 0,
        };
    }

    /// Starts moving from the current level to `to` over exactly `frames`
    /// frames (one when `frames` is 0 and the level changes), so the last
    /// of them is exactly `to`.
    pub(crate) fn retarget_over(&mut self, to: f32, frames: u64) {
        let from = self.level();
        let len = if from == to { 0 } else { frames.max(1) };
        *self = Self {
            from,
            to,
            len,
            pos: 0,
        };
    }

    /// Moves `frames` frames on without producing them.
    pub(crate) fn skip(&mut self, frames: u64) {
        self.pos = (self.pos + frames).min(self.len);
    }

    /// Lands on the target at once.
    pub(crate) fn finish(&mut self) {
        *self = Self::at(self.to);
    }

    fn value(&self, pos: u64) -> f32 {
        if pos >= self.len {
            self.to
        } else if pos == 0 {
            self.from
        } else {
            let t = pos as f64 / self.len as f64;
            (f64::from(self.from) + (f64::from(self.to) - f64::from(self.from)) * t) as f32
        }
    }
}

//! EDIT mode core: a track arranged as an edit-decision list (EDL) over its decoded PCM. Nothing is copied:
//! the arrangement is a sequence of [`Seg`]s, each a frame range of the source or silence, and every edit
//! (cut, copy, paste, insert, delete, clear, duplicate, insert silence) is one [`Edl::splice`]. Undo / redo
//! keep whole EDL snapshots (a few dozen words each). Positions are interleaved-stereo *frames* at the
//! source's rate.
//!
//! Model: Copy, Paste ("pastes from the selected
//! position"), Insert ("inserts to the selected position"), Delete, Clear ("mutes the selected range"),
//! Clone, Undo / Redo, "Render audio files" into the collection. Our semantics for those
//! words: paste overwrites from the position, insert shifts the rest, clear keeps the length.
//!
//! Children: [`snap`] (the edited beat grid and snapping to beats / bars), [`load`] (decode + analysis of
//! the source), [`play`] (preview of the arrangement on the default output), [`wav`] (render to a new WAV file).

pub mod load;
pub mod play;
pub mod snap;
pub mod wav;

#[cfg(test)]
mod tests;

use crate::audio::Pcm;
use std::{ops::Range, sync::Arc};

/// One run of the arrangement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seg {
    /// `len` source frames from `start`.
    Src {
        start: usize,
        len: usize,
    },
    Silence(usize),
}

impl Seg {
    pub fn len(&self) -> usize {
        match *self {
            Seg::Src { len, .. } | Seg::Silence(len) => len,
        }
    }

    /// Frames `a..b` of this segment (`a <= b <= len`).
    fn part(&self, a: usize, b: usize) -> Seg {
        match *self {
            Seg::Src { start, .. } => Seg::Src {
                start: start + a,
                len: b - a,
            },
            Seg::Silence(_) => Seg::Silence(b - a),
        }
    }
}

/// Appends `s` unless empty, joining it to the last segment when they are contiguous.
fn push(out: &mut Vec<Seg>, s: Seg) {
    match (out.last_mut(), s) {
        (_, s) if s.len() == 0 => {}
        (Some(Seg::Silence(a)), Seg::Silence(b)) => *a += b,
        (Some(Seg::Src { start, len }), Seg::Src { start: s2, len: l2 }) if *start + *len == s2 => {
            *len += l2
        }
        _ => out.push(s),
    }
}

/// The arrangement: segments in play order, none empty, contiguous neighbours merged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Edl(Vec<Seg>);

impl Edl {
    /// The unedited track of `frames` frames.
    pub fn whole(frames: usize) -> Edl {
        Edl(if frames == 0 {
            vec![]
        } else {
            vec![Seg::Src {
                start: 0,
                len: frames,
            }]
        })
    }

    pub fn segs(&self) -> &[Seg] {
        &self.0
    }

    /// Frames in the arrangement.
    pub fn len(&self) -> usize {
        self.0.iter().map(Seg::len).sum()
    }

    /// The arrangement's frames `r` (clamped) as segments.
    pub fn slice(&self, r: Range<usize>) -> Vec<Seg> {
        let mut out = Vec::new();
        let mut at = 0;
        for s in &self.0 {
            let (a, b) = (r.start.max(at), r.end.min(at + s.len()));
            if a < b {
                push(&mut out, s.part(a - at, b - at));
            }
            at += s.len();
        }
        out
    }

    /// Replaces frames `r` (clamped to the arrangement) by `with`: delete is `with = []`, insert is an empty `r`.
    pub fn splice(&mut self, r: Range<usize>, with: &[Seg]) {
        let len = self.len();
        let (a, b) = (r.start.min(len), r.end.min(len).max(r.start.min(len)));
        let mut out = self.slice(0..a);
        with.iter().for_each(|&s| push(&mut out, s));
        self.slice(b..len)
            .into_iter()
            .for_each(|s| push(&mut out, s));
        self.0 = out;
    }

    /// Source frame heard at arrangement frame `pos`; `None` in silence or past the end.
    pub fn src_at(&self, pos: usize) -> Option<usize> {
        let mut at = 0;
        for s in &self.0 {
            if pos < at + s.len() {
                return if let Seg::Src { start, .. } = *s {
                    Some(start + pos - at)
                } else {
                    None
                };
            }
            at += s.len();
        }
        None
    }

    /// Fills `out` (interleaved stereo) with the arrangement from frame `pos`; frames past the end are zero.
    /// Returns how many frames of `out` came from the arrangement.
    pub fn read(&self, src: &[f32], pos: usize, out: &mut [f32]) -> usize {
        out.fill(0.0);
        let want = pos..pos + out.len() / 2;
        let mut at = 0;
        let mut got = 0;
        for s in &self.0 {
            let (a, b) = (want.start.max(at), want.end.min(at + s.len()));
            if a < b {
                if let Seg::Src { start, .. } = *s {
                    let from = start + a - at;
                    out[2 * (a - want.start)..2 * (b - want.start)]
                        .copy_from_slice(&src[2 * from..2 * (from + b - a)]);
                }
                got += b - a;
            }
            at += s.len();
        }
        got
    }

    /// Per-frame data `per_frame` entries per frame (a waveform at 150 entries/s: `150 / rate`) laid out as
    /// the arrangement plays it; silence is `T::default()`.
    pub fn remap<T: Copy + Default>(&self, entries: &[T], per_frame: f64) -> Vec<T> {
        let n = (self.len() as f64 * per_frame).ceil() as usize;
        let mut out = vec![T::default(); n];
        let mut at = 0;
        for s in &self.0 {
            if let Seg::Src { start, len } = *s {
                let (j0, j1) = (
                    (at as f64 * per_frame).round() as usize,
                    (((at + len) as f64 * per_frame).round() as usize).min(n),
                );
                for (j, o) in out.iter_mut().enumerate().take(j1).skip(j0) {
                    let frame = start as f64
                        + ((j as f64 + 0.5) / per_frame - at as f64).clamp(0.0, len as f64 - 1.0);
                    if let Some(e) = entries.get((frame * per_frame) as usize) {
                        *o = *e;
                    }
                }
            }
            at += s.len();
        }
        out
    }
}

/// A source track with its arrangement, undo / redo history and the clipboard.
pub struct Edit {
    pcm: Arc<Pcm>,
    edl: Edl,
    undo: Vec<Edl>,
    redo: Vec<Edl>,
    clip: Vec<Seg>,
}

impl Edit {
    pub fn new(pcm: Arc<Pcm>) -> Edit {
        let edl = Edl::whole(pcm.samples.len() / 2);
        Edit {
            pcm,
            edl,
            undo: vec![],
            redo: vec![],
            clip: vec![],
        }
    }

    pub fn pcm(&self) -> &Arc<Pcm> {
        &self.pcm
    }

    pub fn edl(&self) -> &Edl {
        &self.edl
    }

    pub fn rate(&self) -> u32 {
        self.pcm.rate
    }

    /// Frames in the arrangement.
    pub fn len(&self) -> usize {
        self.edl.len()
    }

    pub fn clip_len(&self) -> usize {
        self.clip.iter().map(Seg::len).sum()
    }

    /// Differs from the source as it came.
    pub fn is_edited(&self) -> bool {
        self.edl != Edl::whole(self.pcm.samples.len() / 2)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Runs `f` on the arrangement; a change is recorded for undo (and drops the redo history). Returns
    /// whether `f` changed anything.
    fn change(&mut self, f: impl FnOnce(&mut Edl)) -> bool {
        let before = self.edl.clone();
        f(&mut self.edl);
        let changed = self.edl != before;
        if changed {
            self.undo.push(before);
            self.redo.clear();
        }
        changed
    }

    /// Frames `r` to the clipboard; false (clipboard unchanged) when `r` is empty.
    pub fn copy(&mut self, r: Range<usize>) -> bool {
        let s = self.edl.slice(r);
        let any = !s.is_empty();
        if any {
            self.clip = s;
        }
        any
    }

    /// Copy, then delete. The selection afterwards is the empty range at the cut.
    pub fn cut(&mut self, r: Range<usize>) -> Option<Range<usize>> {
        self.copy(r.clone())
            .then(|| self.delete(r.clone()))
            .flatten()
    }

    /// Removes `r`, the rest moves up.
    pub fn delete(&mut self, r: Range<usize>) -> Option<Range<usize>> {
        self.change(|e| e.splice(r.clone(), &[]))
            .then_some(r.start..r.start)
    }

    /// Mutes `r` (length kept).
    pub fn clear(&mut self, r: Range<usize>) -> Option<Range<usize>> {
        let r = r.start.min(self.len())..r.end.min(self.len());
        self.change(|e| e.splice(r.clone(), &[Seg::Silence(r.len())]))
            .then_some(r)
    }

    /// Overwrites from `at` with the clipboard (extends the arrangement past its end).
    pub fn paste(&mut self, at: usize) -> Option<Range<usize>> {
        let (clip, at) = (self.clip.clone(), at.min(self.len()));
        let n = self.clip_len();
        self.change(|e| e.splice(at..at + n, &clip))
            .then_some(at..at + n)
    }

    /// Inserts the clipboard at `at`, the rest moves down.
    pub fn insert(&mut self, at: usize) -> Option<Range<usize>> {
        let (clip, at) = (self.clip.clone(), at.min(self.len()));
        self.change(|e| e.splice(at..at, &clip))
            .then_some(at..at + self.clip_len())
    }

    /// Inserts `frames` of silence at `at`.
    pub fn insert_silence(&mut self, at: usize, frames: usize) -> Option<Range<usize>> {
        let at = at.min(self.len());
        self.change(|e| e.splice(at..at, &[Seg::Silence(frames)]))
            .then_some(at..at + frames)
    }

    /// Plays `r` twice: a copy goes right behind it (the clipboard is not touched). Returns the copy.
    pub fn duplicate(&mut self, r: Range<usize>) -> Option<Range<usize>> {
        let r = r.start.min(self.len())..r.end.min(self.len());
        let s = self.edl.slice(r.clone());
        self.change(|e| e.splice(r.end..r.end, &s))
            .then_some(r.end..r.end + r.len())
    }

    pub fn undo(&mut self) -> bool {
        self.undo
            .pop()
            .map(|e| self.redo.push(std::mem::replace(&mut self.edl, e)))
            .is_some()
    }

    pub fn redo(&mut self) -> bool {
        self.redo
            .pop()
            .map(|e| self.undo.push(std::mem::replace(&mut self.edl, e)))
            .is_some()
    }

    /// The arrangement as audio (a fresh buffer).
    pub fn render(&self) -> Pcm {
        let mut samples = vec![0.0; self.len() * 2];
        self.edl.read(&self.pcm.samples, 0, &mut samples);
        Pcm {
            rate: self.pcm.rate,
            samples,
        }
    }
}

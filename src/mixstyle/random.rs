//! RANDOM: the style of a transition, drawn from the styles the DJ lets it use.

use std::hash::{BuildHasher, Hasher};

/// One of `enabled` (style indices) for the next transition, never `last` while another is enabled; `r` is any random
/// number ([`random_u64`]). `None` when nothing is enabled.
pub fn pick(enabled: &[usize], last: Option<usize>, r: u64) -> Option<usize> {
    let others = enabled.iter().filter(|&&i| Some(i) != last).count();
    let from = if others > 0 { others } else { enabled.len() };
    let k = r.checked_rem(from as u64)? as usize;
    if others > 0 {
        enabled.iter().filter(|&&i| Some(i) != last).nth(k).copied()
    } else {
        enabled.get(k).copied()
    }
}

/// A fresh random number from the standard library's per-process random hash keys.
pub fn random_u64() -> u64 {
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

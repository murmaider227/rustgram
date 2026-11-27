//! Deterministic panic-fuzz helpers for tests over untrusted-input entry points (TGB-3065).
//!
//! A fixed-seed xorshift generator builds strings from parser-relevant tokens (URL pieces,
//! markdown markers, multibyte and combining characters) and byte buffers from random data or
//! mutated seeds. [`Fuzz::check`] runs a closure under `catch_unwind` and records each distinct
//! panic location once; [`Fuzz::finish`] fails the test with the list. A failure reproduces with
//! the same seed and iteration number. Dev-dependency only; not part of the app.
//!
//! [`tl`] writes random well-formed TL values from the schema for the typed converters, and
//! [`guard`] runs cases on a worker that also catches hangs and huge allocations (TGB-3086).

pub mod guard;
pub mod tl;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Once;

/// Xorshift64 generator: fast, deterministic, good enough for input generation.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    /// Creates a generator; a zero seed is replaced by a fixed non-zero one.
    pub fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    /// Next raw 64-bit value.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Uniform-ish value in `0..n` (`0` when `n == 0`).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }

    /// True with probability `1 / n`.
    pub fn one_in(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }

    /// A random element of `items`.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    /// An integer that is an edge value one time in three, else in `0..=limit + 2`.
    pub fn int(&mut self, limit: usize) -> i64 {
        const EDGES: &[i64] = &[
            0,
            1,
            -1,
            2,
            -2,
            7,
            255,
            256,
            65_535,
            65_536,
            i32::MAX as i64,
            i32::MIN as i64,
            u32::MAX as i64,
            i64::MAX,
            i64::MIN,
        ];
        if self.one_in(3) {
            *self.pick(EDGES)
        } else {
            self.below(limit.saturating_add(3)) as i64
        }
    }
}

/// Tokens shared by every text generator: separators, URL pieces, markdown markers,
/// multibyte, combining, joiner, bidi and control characters.
pub const COMMON_TOKENS: &[&str] = &[
    "a",
    "Z",
    "0",
    "9",
    " ",
    "\n",
    "\r",
    "\t",
    ".",
    ",",
    ":",
    ";",
    "/",
    "\\",
    "@",
    "#",
    "$",
    "%",
    "%2",
    "%zz",
    "%41",
    "&",
    "?",
    "=",
    "*",
    "**",
    "_",
    "__",
    "~~",
    "||",
    "`",
    "```",
    "[",
    "]",
    "](",
    "(",
    ")",
    "{",
    "}",
    "<",
    ">",
    "\"",
    "'",
    "-",
    "+",
    "!",
    "http://",
    "https://",
    "tg://",
    "t.me/",
    "www.",
    ".com",
    ".ton",
    "a@b.c",
    "é",
    "€",
    "ß",
    "İ",
    "𝄞",
    "😀",
    "👍🏽",
    "\u{200d}",
    "\u{fe0f}",
    "\u{0301}",
    "\u{202e}",
    "\u{feff}",
    "🇺🇦",
    "\u{0}",
    "\u{7f}",
    "Ꙋ",
    "ﬃ",
    "123",
    "-1",
    "0x",
    "4294967296",
    "99999999999999999999",
    "1e400",
    "NaN",
    "null",
    "true",
];

/// Collects distinct panic locations across [`Fuzz::check`] calls.
pub struct Fuzz {
    rng: Rng,
    iterations: usize,
    failures: BTreeMap<String, String>,
}

thread_local! {
    static LAST_PANIC: RefCell<String> = const { RefCell::new(String::new()) };
    static SILENT: RefCell<bool> = const { RefCell::new(false) };
}

fn install_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let location = info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()))
                .unwrap_or_default();
            LAST_PANIC.with(|p| p.borrow_mut().clone_from(&location));
            if !SILENT.with(|s| *s.borrow()) {
                previous(info);
            }
        }));
    });
}

impl Fuzz {
    /// Creates a run with a fixed seed. The iteration count defaults to `default_iterations`;
    /// `TGFORK_FUZZ_ITERATIONS` overrides it for a deeper local run.
    pub fn new(seed: u64, default_iterations: usize) -> Self {
        install_hook();
        let iterations = std::env::var("TGFORK_FUZZ_ITERATIONS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default_iterations);
        Self {
            rng: Rng::new(seed),
            iterations,
            failures: BTreeMap::new(),
        }
    }

    /// Number of iterations the caller should run.
    pub fn iterations(&self) -> usize {
        self.iterations
    }

    /// The generator.
    pub fn rng(&mut self) -> &mut Rng {
        &mut self.rng
    }

    /// A string of up to `max_tokens` tokens from [`COMMON_TOKENS`] and `extra`.
    pub fn text(&mut self, extra: &[&str], max_tokens: usize) -> String {
        let n = self.rng.below(max_tokens + 1);
        let mut s = String::new();
        for _ in 0..n {
            let use_extra = !extra.is_empty() && self.rng.one_in(2);
            s.push_str(if use_extra {
                self.rng.pick(extra)
            } else {
                self.rng.pick(COMMON_TOKENS)
            });
        }
        s
    }

    /// Random bytes, up to `max_len` long.
    pub fn bytes(&mut self, max_len: usize) -> Vec<u8> {
        let len = self.rng.below(max_len + 1);
        (0..len).map(|_| self.rng.next_u64() as u8).collect()
    }

    /// `seed` with a few random byte edits: flips, overwrites with edge bytes, truncation,
    /// insertion and duplication of a chunk.
    pub fn mutate(&mut self, seed: &[u8]) -> Vec<u8> {
        let mut out = seed.to_vec();
        let edits = 1 + self.rng.below(4);
        for _ in 0..edits {
            match self.rng.below(6) {
                0 if !out.is_empty() => {
                    let i = self.rng.below(out.len());
                    out[i] ^= 1 << self.rng.below(8);
                }
                1 if !out.is_empty() => {
                    let i = self.rng.below(out.len());
                    out[i] = *self.rng.pick(&[0x00, 0x01, 0x7f, 0x80, 0xfe, 0xff]);
                }
                2 => {
                    let len = self.rng.below(out.len() + 1);
                    out.truncate(len);
                }
                3 => {
                    let i = self.rng.below(out.len() + 1);
                    let b = self.rng.next_u64() as u8;
                    out.insert(i, b);
                }
                4 if !out.is_empty() => {
                    let start = self.rng.below(out.len());
                    let end = start + self.rng.below(out.len() - start + 1);
                    let chunk = out[start..end].to_vec();
                    let at = self.rng.below(out.len() + 1);
                    out.splice(at..at, chunk);
                }
                _ if out.len() >= 4 => {
                    let i = self.rng.below(out.len() - 3);
                    let v = (self.rng.int(1 << 20) as u32).to_le_bytes();
                    out[i..i + 4].copy_from_slice(&v);
                }
                _ => {}
            }
        }
        out
    }

    /// Runs `f`; on a panic records its location with `name`, the iteration and `input`.
    pub fn check(&mut self, name: &str, iteration: usize, input: &dyn Debug, f: impl FnOnce()) {
        SILENT.with(|s| *s.borrow_mut() = true);
        let result = catch_unwind(AssertUnwindSafe(f));
        SILENT.with(|s| *s.borrow_mut() = false);
        if result.is_err() {
            let at = LAST_PANIC.with(|p| p.borrow().clone());
            self.failures.entry(at).or_insert_with(|| {
                let mut shown = format!("{input:?}");
                if shown.len() > 600 {
                    let cut = (0..=600)
                        .rev()
                        .find(|&i| shown.is_char_boundary(i))
                        .unwrap_or(0);
                    shown.truncate(cut);
                    shown.push_str("...");
                }
                format!("{name} at iteration {iteration} on {shown}")
            });
        }
    }

    /// Fails the test with every distinct panic location found.
    pub fn finish(self) {
        let report: Vec<String> = self
            .failures
            .iter()
            .map(|(at, what)| format!("{at}\n    {what}"))
            .collect();
        assert!(report.is_empty(), "panics found:\n{}", report.join("\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_distinct_panic_locations_and_keeps_going() {
        let mut fuzz = Fuzz::new(1, 10);
        for i in 0..fuzz.iterations() {
            fuzz.check("boom", i, &i, || {
                let v: Vec<u8> = Vec::new();
                let _ = v[i];
            });
        }
        assert_eq!(fuzz.failures.len(), 1);
    }

    #[test]
    fn generators_are_deterministic() {
        let mut a = Fuzz::new(7, 1);
        let mut b = Fuzz::new(7, 1);
        assert_eq!(a.text(&["x"], 20), b.text(&["x"], 20));
        assert_eq!(a.mutate(b"seed bytes"), b.mutate(b"seed bytes"));
    }
}

// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stage E1b tests (ced E1 plan §6): multi-chunk sources, random seeks equal a
//! linear parse (also after an edit + `invalidate_from`), time-sliced
//! advancing, and `MAX_LINE_LEN`.

use proptest::prelude::*;
use std::cell::Cell;
use syntax::LineSource;
use syntax::cache::{Cache, INTERVAL};
use syntax::highlighter::{Highlighter, MAX_LINE_LEN, Span};
use syntax::runtime::Language;

fn rust() -> &'static Language {
    syntax::language_by_id("rust").expect("lsh defines rust")
}

/// A source whose chunks end at every multiple of `size` — a line may span
/// many of them, as a gap buffer's does around the gap.
struct Chunked<'a> {
    data: &'a [u8],
    size: usize,
}

impl LineSource for Chunked<'_> {
    fn read_forward(&self, offset: usize) -> &[u8] {
        if offset >= self.data.len() {
            return &[];
        }
        let end = ((offset / self.size) + 1) * self.size;
        &self.data[offset..end.min(self.data.len())]
    }
}

struct Indexed<'a> {
    source: Chunked<'a>,
    ends: Vec<usize>,
    reads: Cell<usize>,
}

impl<'a> Indexed<'a> {
    fn new(data: &'a [u8], size: usize) -> Self {
        let mut ends: Vec<_> = data
            .iter()
            .enumerate()
            .filter_map(|(i, &byte)| (byte == b'\n').then_some(i + 1))
            .collect();
        ends.push(data.len());
        Self {
            source: Chunked { data, size },
            ends,
            reads: Cell::new(0),
        }
    }
}

impl LineSource for Indexed<'_> {
    fn read_forward(&self, offset: usize) -> &[u8] {
        self.reads.set(self.reads.get() + 1);
        self.source.read_forward(offset)
    }

    fn indexed_line_end(&self, line: usize) -> Option<usize> {
        self.ends.get(line.checked_sub(1)?).copied()
    }
}

/// Every line's spans, parsed top to bottom by one highlighter.
fn linear(src: &dyn LineSource, lines: usize) -> Vec<Vec<Span>> {
    let mut h = Highlighter::new(src, rust());
    (0..lines)
        .map(|_| {
            let mut out = Vec::new();
            h.parse_next_line(&mut out);
            out
        })
        .collect()
}

const POOL: &[&str] = &[
    "fn main() {",
    "    let x = \"a string // not a comment\";",
    "    // a line comment with \"quotes\"",
    "    /* a block comment opens",
    "       and continues here",
    "    */ let after = 42;",
    "}",
    "",
    "#[derive(Debug)]",
    "pub struct S<'a> { r: &'a str, n: u64 }",
    "    let s = r#\"raw \" string\"#;",
    "    let c = 'c'; let e = '\\n';",
    "impl S<'_> { pub fn n(&self) -> u64 { self.n * 0x10 + 1_000 } }",
    "    \"unterminated string continues",
    "    onto this line\";",
    "\t// tab-indented comment\r",
];

fn build(picks: &[usize]) -> String {
    let mut text = String::new();
    for &p in picks {
        text.push_str(POOL[p % POOL.len()]);
        text.push('\n');
    }
    text
}

#[test]
fn spans_are_absolute_and_classified() {
    let text = "fn main() {\n    // hello\n}\n";
    let src = text.as_bytes();
    let spans = linear(&src, 3);
    let comment = spans[1]
        .iter()
        .find(|s| format!("{:?}", s.kind).contains("Comment"))
        .expect("line 2 has a comment span");
    assert_eq!(comment.start, text.find("//").unwrap());
    for line in &spans {
        assert!(
            line.windows(2).all(|w| w[0].start < w[1].start),
            "spans are ordered: {line:?}"
        );
    }
    assert!(spans[0].iter().all(|s| s.start < text.find('\n').unwrap()));
}

#[test]
fn multi_chunk_sources_equal_one_slice() {
    let picks: Vec<usize> = (0..400).map(|i| (i * 7 + i / 3) % POOL.len()).collect();
    let text = build(&picks);
    let flat = text.as_bytes();
    let want = linear(&flat, picks.len() + 1);
    for size in [1, 2, 3, 7, 64, 4096] {
        let src = Chunked {
            data: text.as_bytes(),
            size,
        };
        assert_eq!(linear(&src, picks.len() + 1), want, "chunk size {size}");
    }
}

#[test]
fn interval_is_pinned() {
    assert_eq!(INTERVAL, 1024);
}

#[test]
fn over_long_lines_are_plain_and_keep_line_numbers() {
    let long = "x".repeat(MAX_LINE_LEN + 8 * 1024);
    let just_under = format!("// {}", "y".repeat(MAX_LINE_LEN - 5));
    let text = format!("// before\n{long}\n// after\n{just_under}\nfn f() {{}}\n");
    let tail = text.find("// after").unwrap();
    let last = text.find("fn f()").unwrap();
    for size in [7, 1000, 1 << 20] {
        let src = Chunked {
            data: text.as_bytes(),
            size,
        };
        let mut h = Highlighter::new(&src, rust());
        let mut out = Vec::new();
        h.parse_next_line(&mut out);
        assert!(!out.is_empty());
        h.parse_next_line(&mut out);
        assert!(
            out.is_empty(),
            "a {}-byte line is not highlighted",
            long.len()
        );
        assert_eq!(h.line(), 3);
        h.parse_next_line(&mut out);
        assert_eq!(
            out.first().map(|s| s.start),
            Some(tail),
            "the line after it starts where it should (chunk {size})"
        );
        h.parse_next_line(&mut out);
        assert!(
            !out.is_empty(),
            "a line one byte short of the cap (newline included) is highlighted"
        );
        h.parse_next_line(&mut out);
        assert_eq!(out.first().map(|s| s.start), Some(last));
        assert_eq!(h.line(), 6);
    }
}

#[test]
fn indexed_long_lines_skip_reads_and_preserve_runtime_and_offsets() {
    for newline in ["\n", "\r\n"] {
        for length in [MAX_LINE_LEN - newline.len(), MAX_LINE_LEN, 2 * 1024 * 1024] {
            let text = format!(
                "/* before{newline}{}{newline}*/ let after = 42;{newline}",
                "x".repeat(length)
            );
            let source = Indexed::new(text.as_bytes(), 7);
            let plain = Chunked {
                data: text.as_bytes(),
                size: 7,
            };
            let want = linear(&plain, 4);
            let mut h = Highlighter::new(&source, rust());
            let mut out = Vec::new();
            h.parse_next_line(&mut out);
            assert_eq!(out, want[0]);
            let reads = source.reads.get();
            h.parse_next_line(&mut out);
            assert!(out.is_empty());
            assert_eq!(source.reads.get(), reads, "no chunk reads for skipped line");
            assert_eq!(h.line(), 3);
            let state = h.snapshot();
            h.parse_next_line(&mut out);
            assert_eq!(out, want[2], "runtime carries across skipped line");
            h.restore(&state);
            h.parse_next_line(&mut out);
            assert_eq!(out, want[2], "checkpoint preserves the true next offset");
        }
    }
    let text = "x".repeat(2 * 1024 * 1024);
    let source = Indexed::new(text.as_bytes(), 7);
    let mut h = Highlighter::new(&source, rust());
    let mut out = Vec::new();
    h.parse_next_line(&mut out);
    assert!(out.is_empty());
    assert_eq!(
        source.reads.get(),
        0,
        "unterminated long line skips reads too"
    );
    h.parse_next_line(&mut out);
    assert!(out.is_empty());
}

#[test]
fn indexed_short_lines_keep_cap_boundary_and_chunk_assembly() {
    let text = format!("// {}\n// next\n", "x".repeat(MAX_LINE_LEN - 5));
    for size in [1, 7, 4096] {
        let source = Indexed::new(text.as_bytes(), size);
        assert_eq!(linear(&source, 3), linear(&source.source, 3));
        assert!(source.reads.get() > 0);
    }
}

#[test]
#[ignore = "release performance probe; run on a build node"]
fn indexed_long_line_benchmark() {
    use std::hint::black_box;
    use std::time::Instant;

    let text = format!("{}\n// after\n", "x".repeat(5 * 1024 * 1024));
    let source = Indexed::new(text.as_bytes(), 4096);
    let mut out = Vec::new();
    for indexed in [false, true] {
        let src: &dyn LineSource = if indexed { &source } else { &source.source };
        let started = Instant::now();
        for _ in 0..1000 {
            let mut h = Highlighter::new(src, rust());
            h.parse_next_line(&mut out);
            h.parse_next_line(&mut out);
            assert_eq!(
                out.first().map(|span| span.start),
                Some(5 * 1024 * 1024 + 1)
            );
            black_box(&out);
        }
        eprintln!(
            "indexed={indexed} iterations=1000 elapsed_us={}",
            started.elapsed().as_micros()
        );
    }
}

#[test]
fn past_the_end_is_empty() {
    let text = "fn a() {}";
    let src = text.as_bytes();
    let mut h = Highlighter::new(&src, rust());
    let mut out = Vec::new();
    h.parse_next_line(&mut out);
    assert!(
        !out.is_empty(),
        "a last line without a newline is highlighted"
    );
    h.parse_next_line(&mut out);
    assert!(out.is_empty());
}

#[test]
fn advance_makes_progress_below_one_interval_per_call() {
    let picks: Vec<usize> = (0..3 * INTERVAL + 10).map(|i| i % POOL.len()).collect();
    let text = build(&picks);
    let src = text.as_bytes();
    let want = linear(&src, picks.len());
    let target = picks.len() - 3;

    let mut cache = Cache::new();
    let mut calls = 0;
    loop {
        // A fresh highlighter per call, as a per-frame caller has.
        let mut h = Highlighter::new(&src, rust());
        calls += 1;
        if cache.advance(&mut h, target, 100) {
            break;
        }
        assert!(calls < 100, "no progress");
    }
    assert!(
        calls >= 3 * INTERVAL / 100,
        "each call parsed at most 100 lines"
    );
    assert!(cache.reach() > (target - 1) / INTERVAL * INTERVAL);

    let mut h = Highlighter::new(&src, rust());
    let mut out = Vec::new();
    cache.parse_line(&mut h, target, &mut out);
    assert_eq!(out, want[target - 1]);
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    #[test]
    fn random_seeks_equal_a_linear_parse(
        picks in proptest::collection::vec(0..POOL.len(), 2 * INTERVAL..3 * INTERVAL),
        seeks in proptest::collection::vec((any::<proptest::sample::Index>(), any::<bool>()), 1..40),
        edit in (any::<proptest::sample::Index>(), 0..POOL.len()),
        chunk in prop_oneof![Just(5usize), Just(333), Just(1 << 20)],
    ) {
        let lines = picks.len() + 1;
        let text = build(&picks);
        let src = Chunked { data: text.as_bytes(), size: chunk };
        let want = linear(&src, lines);

        let mut cache = Cache::new();
        let mut kept = Highlighter::new(&src, rust());
        let mut out = Vec::new();
        for (at, fresh) in &seeks {
            let line = at.index(lines) + 1;
            if *fresh {
                let mut h = Highlighter::new(&src, rust());
                cache.parse_line(&mut h, line, &mut out);
            } else {
                cache.parse_line(&mut kept, line, &mut out);
            }
            prop_assert_eq!(&out, &want[line - 1], "line {}", line);
        }

        // Edit one line, invalidate from it, and seek again.
        let mut edited = picks.clone();
        let (at, with) = edit;
        let changed = at.index(edited.len());
        edited[changed] = with;
        let text2 = build(&edited);
        let src2 = Chunked { data: text2.as_bytes(), size: chunk };
        let want2 = linear(&src2, lines);
        cache.invalidate_from(changed + 1);
        for (at, _) in &seeks {
            let line = at.index(lines) + 1;
            let mut h = Highlighter::new(&src2, rust());
            cache.parse_line(&mut h, line, &mut out);
            prop_assert_eq!(&out, &want2[line - 1], "after the edit, line {}", line);
        }
    }
}

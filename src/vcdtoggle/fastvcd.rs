//! Specialised single-pass VCD body scanner.
//!
//! wellen's generic body parser walks the file byte by byte and decodes every
//! value of the included signals into an owned buffer. For the toggle metrics
//! that is far more work than needed. This scanner memory-maps the file,
//! skips the lines of uninteresting signals with a single hash lookup on the
//! identifier, and compares the values of the selected signals in a bit-packed
//! 4-state form without any allocation.
//!
//! It deliberately handles only the strict, line-oriented subset of VCD that
//! simulators emit (one value change per line, 2/4-state bit vectors,
//! non-decreasing timestamps). Anything else yields an error and the caller
//! falls back to wellen's streaming engine, so results never depend on this
//! scanner guessing.

use memmap2::{Advice, Mmap};
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use wellen::{Hierarchy, SignalEncoding, SignalRef, Time};

pub struct SignalCounts {
    pub signal: SignalRef,
    pub toggles: u64,
    pub toggled_bits: u32,
}

pub struct FastOutcome {
    pub counts: Vec<SignalCounts>,
    pub last_change_time: Option<Time>,
}

type FastResult<T> = Result<T, String>;

/// Longest identifier that fits the packed 64-bit lookup key.
const MAX_ID_LEN: usize = 8;
const NO_PREV: u8 = 4;

pub fn run(path: &Path, hierarchy: &Hierarchy, signals: &[SignalRef]) -> FastResult<FastOutcome> {
    let file = File::open(path)
        .map_err(|err| format!("cannot open input file '{}': {err}", path.display()))?;
    // SAFETY: the file is only read; like any mmap-based reader this assumes it
    // is not truncated by another process while being evaluated.
    let mmap = unsafe { Mmap::map(&file) }
        .map_err(|err| format!("cannot memory-map input file '{}': {err}", path.display()))?;
    let _ = mmap.advise(Advice::Sequential);

    let body_start = locate_body(&mmap).ok_or("no $enddefinitions found")?;
    let mut scanner = Scanner::prepare(hierarchy, signals, &mmap[..body_start])?;
    scanner.scan(&mmap[body_start..])?;
    Ok(scanner.finish(signals))
}

/// Offset right after `$enddefinitions $end`.
fn locate_body(buf: &[u8]) -> Option<usize> {
    const KEYWORD: &[u8] = b"$enddefinitions";
    let start = memchr::memmem::find(buf, KEYWORD)? + KEYWORD.len();
    let end = memchr::memmem::find(&buf[start..], b"$end")?;
    Some(start + end + 4)
}

#[derive(Clone, Copy)]
struct Slot {
    toggles: u64,
    /// Word offset into the arena (wide signals only).
    off: u32,
    width: u32,
    /// Previous code of a 1-bit signal (0, 1, x = 2, z = 3) or `NO_PREV`.
    prev: u8,
    /// Wide signals: a previous value exists.
    init: bool,
    /// 1-bit signals: the bit ever changed.
    changed: bool,
}

/// Identifier to slot lookup. Simulators number identifiers in declaration
/// order, so the identifiers of one scope usually span a narrow range and a
/// directly indexed table is both the fastest and the most cache-friendly;
/// a small open-addressing hash table covers sparse identifier sets.
struct IdIndex {
    min: u64,
    dense: Vec<u32>,
    keys: Vec<u64>,
    slots: Vec<u32>,
    shift: u32,
    mask: usize,
}

const NO_SLOT: u32 = u32::MAX;

impl IdIndex {
    fn build(entries: &[(u64, u32)]) -> FastResult<Self> {
        let min = entries.iter().map(|e| e.0).min().unwrap_or(0);
        let max = entries.iter().map(|e| e.0).max().unwrap_or(0);
        let span = (max - min) as usize + 1;
        let mut index = IdIndex {
            min,
            dense: Vec::new(),
            keys: Vec::new(),
            slots: Vec::new(),
            shift: 0,
            mask: 0,
        };
        if span <= entries.len() * 16 + 4096 {
            index.dense = vec![NO_SLOT; span];
            for &(key, slot) in entries {
                let cell = &mut index.dense[(key - min) as usize];
                if *cell != NO_SLOT && *cell != slot {
                    return Err("identifier shared by different signals".into());
                }
                *cell = slot;
            }
        } else {
            let capacity = (entries.len() * 2).next_power_of_two().max(16);
            index.keys = vec![0; capacity];
            index.slots = vec![NO_SLOT; capacity];
            index.shift = 64 - capacity.trailing_zeros();
            index.mask = capacity - 1;
            for &(key, slot) in entries {
                let mut i = index.bucket(key);
                while index.keys[i] != 0 && index.keys[i] != key {
                    i = (i + 1) & index.mask;
                }
                if index.keys[i] == key && index.slots[i] != slot {
                    return Err("identifier shared by different signals".into());
                }
                index.keys[i] = key;
                index.slots[i] = slot;
            }
        }
        Ok(index)
    }

    #[inline(always)]
    fn bucket(&self, key: u64) -> usize {
        (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> self.shift) as usize
    }

    #[inline(always)]
    fn get(&self, key: u64) -> u32 {
        if !self.dense.is_empty() {
            let i = key.wrapping_sub(self.min) as usize;
            return self.dense.get(i).copied().unwrap_or(NO_SLOT);
        }
        let mut i = self.bucket(key);
        loop {
            let k = self.keys[i];
            if k == key {
                return self.slots[i];
            }
            if k == 0 {
                return NO_SLOT;
            }
            i = (i + 1) & self.mask;
        }
    }
}

struct Scanner {
    slots: Vec<Slot>,
    /// Per wide signal: previous value (two planes) and changed-bit mask.
    arena: Vec<u64>,
    index: IdIndex,
    scratch: Vec<u8>,
    last_time: Option<Time>,
}

impl Scanner {
    /// Matches the `$var` declarations of the header to the hierarchy and
    /// builds the identifier lookup of the selected signals.
    fn prepare(
        hierarchy: &Hierarchy,
        signals: &[SignalRef],
        header: &[u8],
    ) -> FastResult<Self> {
        let mut declarations: Vec<(&[u8], &[u8])> = Vec::new();
        let mut tokens = header
            .split(|b| b.is_ascii_whitespace())
            .filter(|t| !t.is_empty());
        while let Some(token) = tokens.next() {
            if token == b"$var" {
                let (_, Some(size), Some(id)) = (tokens.next(), tokens.next(), tokens.next())
                else {
                    return Err("truncated $var".into());
                };
                declarations.push((size, id));
            }
        }

        let mut slot_of: HashMap<SignalRef, u32> = HashMap::with_capacity(signals.len());
        let mut slots = Vec::with_capacity(signals.len());
        let mut arena_words = 0usize;
        for &signal in signals {
            let width = match hierarchy.get_signal_tpe(signal) {
                Some(SignalEncoding::BitVector(width)) if width > 0 => width,
                _ => return Err("selected signal is not a bit vector".into()),
            };
            let nwords = width.div_ceil(64) as usize;
            let off = u32::try_from(arena_words).map_err(|_| "too many signal bits")?;
            if width > 1 {
                arena_words += 3 * nwords;
            }
            slot_of.insert(signal, slots.len() as u32);
            slots.push(Slot {
                toggles: 0,
                off,
                width,
                prev: NO_PREV,
                init: false,
                changed: false,
            });
        }

        let mut ids: Vec<(u64, u32)> = Vec::with_capacity(signals.len());

        let mut vars = hierarchy.all_vars();
        for (size, id) in &declarations {
            let var_ref = vars.next().ok_or("more $var than variables")?;
            let var = &hierarchy[var_ref];
            let Some(&slot) = slot_of.get(&var.signal_ref()) else {
                continue;
            };
            let declared: u32 = std::str::from_utf8(size)
                .ok()
                .and_then(|s| s.parse().ok())
                .ok_or("bad $var size")?;
            if var.length(hierarchy) != Some(declared) {
                return Err("$var size differs from hierarchy".into());
            }
            if id.len() > MAX_ID_LEN || id.iter().any(|&b| b.wrapping_sub(33) > 93) {
                return Err("unsupported identifier".into());
            }
            ids.push((id_key(id), slot));
        }
        if vars.next().is_some() {
            return Err("fewer $var than variables".into());
        }
        Ok(Scanner {
            slots,
            arena: vec![0; arena_words],
            index: IdIndex::build(&ids)?,
            scratch: Vec::new(),
            last_time: None,
        })
    }

    fn scan(&mut self, buf: &[u8]) -> FastResult<()> {
        let n = buf.len();
        let mut pos = 0;
        let mut time: Time = 0;
        while pos < n {
            match buf[pos] {
                b'\n' => pos += 1,
                b'#' => {
                    let start = pos + 1;
                    let mut p = start;
                    let mut value: Time = 0;
                    while p < n && buf[p].is_ascii_digit() {
                        value = value
                            .checked_mul(10)
                            .and_then(|v| v.checked_add(Time::from(buf[p] - b'0')))
                            .ok_or("timestamp overflow")?;
                        p += 1;
                    }
                    if p == start || (p < n && buf[p] != b'\n') {
                        return Err("unsupported timestamp line".into());
                    }
                    if value < time {
                        return Err("time decreases".into());
                    }
                    time = value;
                    pos = p + 1;
                }
                c @ (b'0' | b'1' | b'x' | b'X' | b'z' | b'Z') => {
                    let (key, next) = scan_id(buf, pos + 1)?;
                    let slot = self.index.get(key);
                    if slot != NO_SLOT {
                        self.last_time = Some(time);
                        self.scalar(slot as usize, c)?;
                    }
                    pos = next;
                }
                kind @ (b'b' | b'B' | b'r' | b'R') => {
                    let rest = &buf[pos + 1..];
                    let split = find_space_or_newline(rest).ok_or("truncated vector")?;
                    if rest[split] != b' ' {
                        return Err("vector without identifier".into());
                    }
                    let (key, next) = scan_id(buf, pos + 1 + split + 1)?;
                    let slot = self.index.get(key);
                    if slot != NO_SLOT {
                        if matches!(kind, b'r' | b'R') {
                            return Err("real value on a bit vector".into());
                        }
                        self.last_time = Some(time);
                        self.vector(slot as usize, rest, split)?;
                    }
                    pos = next;
                }
                b'$' => {
                    let end = memchr::memchr(b'\n', &buf[pos..]).map_or(n, |e| pos + e);
                    match &buf[pos..end] {
                        b"$dumpvars" | b"$end" | b"$dumpoff" | b"$dumpon" | b"$dumpall" => {}
                        _ => return Err("unsupported command in body".into()),
                    }
                    pos = end + 1;
                }
                _ => return Err("unsupported line in body".into()),
            }
        }
        Ok(())
    }

    /// Value change given as `<0|1|x|z><id>`.
    #[inline(always)]
    fn scalar(&mut self, slot: usize, c: u8) -> FastResult<()> {
        if self.slots[slot].width == 1 {
            self.bit(slot, c);
            Ok(())
        } else {
            self.vector_value(slot, &[c], 1)
        }
    }

    /// Value change given as `b<bits> <id>`; `src[..len]` is the text after the
    /// `b`, `src` extends to the end of the file.
    #[inline(always)]
    fn vector(&mut self, slot: usize, src: &[u8], len: usize) -> FastResult<()> {
        if self.slots[slot].width == 1 {
            // wellen takes the last character; an empty value counts as zero
            let c = if len == 0 { b'0' } else { src[len - 1] };
            if !matches!(c, b'0' | b'1' | b'x' | b'X' | b'z' | b'Z') {
                return Err("invalid bit value".into());
            }
            self.bit(slot, c);
            Ok(())
        } else {
            self.vector_value(slot, src, len)
        }
    }

    #[inline(always)]
    fn bit(&mut self, slot: usize, c: u8) {
        let code = (((c | (c >> 1)) & 1) | ((c >> 5) & 2)) & 3;
        let s = &mut self.slots[slot];
        if s.prev != NO_PREV && s.prev != code {
            s.toggles += 1;
            s.changed = true;
        }
        s.prev = code;
    }

    fn vector_value(&mut self, slot: usize, src: &[u8], len: usize) -> FastResult<()> {
        let width = self.slots[slot].width as usize;
        let first = *src[..len].first().ok_or("empty vector value")?;
        if len == width {
            return self.update(slot, src, len);
        }
        if len > width {
            return Err("vector value wider than signal".into());
        }
        // left-extend: 0/1 with zero, x/z with themselves
        let pad = match first {
            b'0' | b'1' => b'0',
            b'x' | b'X' | b'z' | b'Z' => first,
            _ => return Err("invalid vector value".into()),
        };
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.clear();
        scratch.resize(width - len, pad);
        scratch.extend_from_slice(&src[..len]);
        let result = self.update(slot, &scratch, width);
        self.scratch = scratch;
        result
    }

    /// Compares the value `src[..len]` (exactly `width` characters; `src` may
    /// extend beyond it) with the previous value.
    #[inline(always)]
    fn update(&mut self, slot: usize, src: &[u8], len: usize) -> FastResult<()> {
        let s = &mut self.slots[slot];
        let nwords = len.div_ceil(64);
        let base = s.off as usize;
        let (prev_v, rest) = self.arena[base..base + 3 * nwords].split_at_mut(nwords);
        let (prev_u, mask) = rest.split_at_mut(nwords);
        let init = s.init;
        let mut changed = 0u64;
        let mut valid = true;
        for i in 0..nwords {
            let (v, u, ok) = encode_word(src, i * 64, len);
            valid &= ok;
            if init {
                let diff = (v ^ prev_v[i]) | (u ^ prev_u[i]);
                changed |= diff;
                mask[i] |= diff;
            }
            prev_v[i] = v;
            prev_u[i] = u;
        }
        if !valid {
            return Err("invalid vector value".into());
        }
        s.toggles += u64::from(changed != 0);
        s.init = true;
        Ok(())
    }

    fn finish(self, signals: &[SignalRef]) -> FastOutcome {
        let counts = self
            .slots
            .iter()
            .zip(signals)
            .map(|(s, &signal)| {
                let toggled_bits = if s.width == 1 {
                    u32::from(s.changed)
                } else {
                    let nwords = s.width.div_ceil(64) as usize;
                    let mask_start = s.off as usize + 2 * nwords;
                    self.arena[mask_start..mask_start + nwords]
                        .iter()
                        .map(|w| w.count_ones())
                        .sum()
                };
                SignalCounts {
                    signal,
                    toggles: s.toggles,
                    toggled_bits,
                }
            })
            .collect();
        FastOutcome {
            counts,
            last_change_time: self.last_time,
        }
    }
}

/// Base-94 value of the identifier; the digits are 1-based, so it is never 0.
#[inline(always)]
fn id_key(id: &[u8]) -> u64 {
    id.iter().rev().fold(0u64, |key, &b| key * 94 + u64::from(b - 32))
}

/// Reads the identifier starting at `pos` up to the end of the line.
/// Returns its key (see `id_key`) and the offset of the next line.
#[inline(always)]
fn scan_id(buf: &[u8], pos: usize) -> FastResult<(u64, usize)> {
    let n = buf.len();
    let mut key = 0u64;
    let mut weight = 1u64;
    let mut p = pos;
    while p < n {
        let b = buf[p];
        if b.wrapping_sub(33) > 93 {
            break;
        }
        if p - pos == MAX_ID_LEN {
            return Err("identifier too long".into());
        }
        key += weight * u64::from(b - 32);
        weight *= 94;
        p += 1;
    }
    if p == pos {
        return Err("missing identifier".into());
    }
    if p < n {
        if buf[p] != b'\n' {
            return Err("unexpected character after identifier".into());
        }
        p += 1;
    }
    Ok((key, p))
}

/// Encodes the characters `src[start..len]` (at most 64) of `0 1 x X z Z` into
/// two bit planes: bit i of `v`/`u` belongs to character `start + i`; the codes
/// are 0 = (0,0), 1 = (1,0), x = (0,1), z = (1,1). The last value tells whether
/// all characters were valid. `src` may extend beyond `len`.
#[inline(always)]
fn encode_word(src: &[u8], start: usize, len: usize) -> (u64, u64, bool) {
    let rem = len - start;
    if src.len() - start >= 64 {
        let (v, u, ok) = encode_64(&src[start..start + 64]);
        if rem >= 64 {
            return (v, u, ok == u64::MAX);
        }
        let keep = (1u64 << rem) - 1;
        (v & keep, u & keep, (ok | !keep) == u64::MAX)
    } else {
        let mut padded = [b'0'; 64];
        padded[..rem].copy_from_slice(&src[start..len]);
        let (v, u, ok) = encode_64(&padded);
        (v, u, ok == u64::MAX)
    }
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn encode_64(chunk: &[u8]) -> (u64, u64, u64) {
    use std::arch::x86_64::*;
    debug_assert_eq!(chunk.len(), 64);
    let (mut v, mut u, mut ok) = (0u64, 0u64, 0u64);
    // SAFETY: SSE2 is part of the x86_64 baseline; each of the four 16-byte
    // unaligned loads stays within the 64-byte chunk.
    unsafe {
        let hi_bit1 = _mm_set1_epi8(0xFEu8 as i8);
        let two = _mm_set1_epi8(0x30);
        let drop_case_and_z = _mm_set1_epi8(0xDDu8 as i8);
        let xz = _mm_set1_epi8(0x58);
        for i in 0..4 {
            let x = _mm_loadu_si128(chunk.as_ptr().add(16 * i) as *const __m128i);
            let u_mask = _mm_movemask_epi8(_mm_slli_epi16(x, 1)) as u16 as u64;
            let v_mask = (_mm_movemask_epi8(_mm_slli_epi16(x, 7))
                | _mm_movemask_epi8(_mm_slli_epi16(x, 6))) as u16 as u64;
            let good = _mm_or_si128(
                _mm_cmpeq_epi8(_mm_and_si128(x, hi_bit1), two),
                _mm_cmpeq_epi8(_mm_and_si128(x, drop_case_and_z), xz),
            );
            let ok_mask = _mm_movemask_epi8(good) as u16 as u64;
            u |= u_mask << (16 * i);
            v |= v_mask << (16 * i);
            ok |= ok_mask << (16 * i);
        }
    }
    (v, u, ok)
}

#[cfg(not(target_arch = "x86_64"))]
#[inline(always)]
fn encode_64(chunk: &[u8]) -> (u64, u64, u64) {
    let (mut v, mut u, mut ok) = (0u64, 0u64, 0u64);
    for (i, &c) in chunk.iter().enumerate() {
        ok |= u64::from(matches!(c, b'0' | b'1' | b'x' | b'X' | b'z' | b'Z')) << i;
        v |= u64::from((c | (c >> 1)) & 1) << i;
        u |= u64::from((c >> 6) & 1) << i;
    }
    (v, u, ok)
}

/// Index of the first space or newline.
#[inline(always)]
fn find_space_or_newline(buf: &[u8]) -> Option<usize> {
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    {
        use std::arch::x86_64::*;
        let mut i = 0;
        // SAFETY: this block is only compiled when AVX2 is enabled; every
        // load stays within `buf`.
        unsafe {
            let space = _mm256_set1_epi8(b' ' as i8);
            let newline = _mm256_set1_epi8(b'\n' as i8);
            while i + 32 <= buf.len() {
                let x = _mm256_loadu_si256(buf.as_ptr().add(i) as *const __m256i);
                let hit = _mm256_or_si256(
                    _mm256_cmpeq_epi8(x, space),
                    _mm256_cmpeq_epi8(x, newline),
                );
                let m = _mm256_movemask_epi8(hit) as u32;
                if m != 0 {
                    return Some(i + m.trailing_zeros() as usize);
                }
                i += 32;
            }
        }
        buf[i..]
            .iter()
            .position(|&b| b == b' ' || b == b'\n')
            .map(|p| i + p)
    }
    #[cfg(all(target_arch = "x86_64", not(target_feature = "avx2")))]
    {
        use std::arch::x86_64::*;
        let mut i = 0;
        // SAFETY: SSE2 is part of the x86_64 baseline; every load stays
        // within `buf`.
        unsafe {
            let space = _mm_set1_epi8(b' ' as i8);
            let newline = _mm_set1_epi8(b'\n' as i8);
            while i + 16 <= buf.len() {
                let x = _mm_loadu_si128(buf.as_ptr().add(i) as *const __m128i);
                let hit = _mm_or_si128(_mm_cmpeq_epi8(x, space), _mm_cmpeq_epi8(x, newline));
                let m = _mm_movemask_epi8(hit) as u32;
                if m != 0 {
                    return Some(i + m.trailing_zeros() as usize);
                }
                i += 16;
            }
        }
        buf[i..]
            .iter()
            .position(|&b| b == b' ' || b == b'\n')
            .map(|p| i + p)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        memchr::memchr2(b' ', b'\n', buf)
    }
}

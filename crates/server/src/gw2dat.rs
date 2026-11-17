//! gw2it-server gw2dat --dat Gw2.dat --seeds seeds.db --out gw2dat_en.db [--db gw2it.db] [--file map_it.db]
//! gw2it-server seeds --out seeds.db      (the seeds, from Text Translator's shared pool: sync_seeds below)
//!
//! Every English text of the game, read from Gw2.dat without anyone walking past it, for bulk.rs
//! (`bulk --file gw2dat_en.db` reads its `texts (key, en)` like the Local Translator's map).
//!
//! The texts are in "strs" files, 1024 texts each; the last two bytes of a file are its language
//! (0 = English) and its block modulo 255, and a text's ID is block * 1024 + its place in the file.
//! About a quarter of the texts are plain (UTF-16); the rest are RC4-encrypted, each with its own
//! seed, which the game server sends when the text is needed. Text Translator keeps the seeds its
//! users have received in `seeds.db` (a pool shared by all its users): with a seed, a text opens
//! without being seen. The full block of a file is not written anywhere: of the few blocks that
//! fit its last byte, it is the one where the file's plain texts match the English we already have
//! under those IDs (the database, the map) and where the seeds open its encrypted texts.
//!
//! Read-only on Gw2.dat (ifalna's copy, kept updated by the official client: gw2-update.sh). The
//! scan of the archive is kept in the output file: the next run reads again only what a patch
//! changed. The output is written to a temporary file and renamed: bulk never reads half of it.
//!
//! The archive format and the decompression are those documented by the community tools
//! (gw2dattools, Gw2Browser); written here from scratch.

use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{Read, Seek, SeekFrom},
};

use rusqlite::{Connection, OptionalExtension, params};

/// Files bigger than this are no strs file (the biggest are a few hundred KB).
const STRS_MAX: u32 = 8 * 1024 * 1024;
/// Seeds tried per candidate block before deciding.
const SEEDS_TRIED: usize = 60;

// ---------------------------------------------------------------- the archive

pub struct Dat {
    file: File,
    mft: Vec<u8>,
    /// MFT index -> the highest file ID that points to it (the file's current ID).
    top: HashMap<u32, u32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub offset: u64,
    pub size: u32,
    pub compressed: bool,
    pub crc: u32,
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

impl Dat {
    pub fn open(path: &str) -> Result<Dat, String> {
        let mut file = File::open(path).map_err(|e| format!("{path}: {e}"))?;
        let mut head = [0u8; 40];
        file.read_exact(&mut head).map_err(|e| format!("{path}: {e}"))?;
        if &head[1..4] != b"AN\x1a" {
            return Err(format!("{path}: not a Gw2.dat"));
        }
        let (mft_offset, mft_size) = (u64_at(&head, 24), u32_at(&head, 32) as usize);
        let mut mft = vec![0u8; mft_size];
        file.seek(SeekFrom::Start(mft_offset)).and_then(|_| file.read_exact(&mut mft)).map_err(|e| format!("{path}: MFT: {e}"))?;
        let mut dat = Dat { file, mft, top: HashMap::new() };
        // Entry 2: the table file ID -> MFT index.
        let mapping = dat.read_raw(dat.entry(2).ok_or("no file table")?, None)?;
        for pair in mapping.chunks_exact(8) {
            let (id, index) = (u32_at(pair, 0), u32_at(pair, 4));
            if id == 0 && index == 0 {
                continue;
            }
            let top = dat.top.entry(index).or_insert(0);
            *top = (*top).max(id);
        }
        Ok(dat)
    }

    pub fn entry(&self, index: u32) -> Option<Entry> {
        let at = index as usize * 24;
        let e = self.mft.get(at..at + 24)?;
        Some(Entry { offset: u64_at(e, 0), size: u32_at(e, 8), compressed: u16_at(e, 12) != 0, crc: u32_at(e, 20) })
    }

    /// The file's bytes, decompressed; with `limit`, only the first `limit` bytes (read from the
    /// first 4 KB of the stored file).
    pub fn read_raw(&mut self, e: Entry, limit: Option<usize>) -> Result<Vec<u8>, String> {
        let take = if limit.is_some() { e.size.min(4096) } else { e.size } as usize;
        let mut raw = vec![0u8; take];
        self.file.seek(SeekFrom::Start(e.offset)).and_then(|_| self.file.read_exact(&mut raw)).map_err(|err| err.to_string())?;
        if e.compressed {
            decompress(&raw, limit)
        } else {
            raw.truncate(limit.unwrap_or(take).min(take));
            Ok(raw)
        }
    }
}

// ---------------------------------------------------------------- decompression

/// Big-endian bit stream over the little-endian 32-bit words of the file; every 64 KB of the
/// stored file a 4-byte checksum, skipped.
struct Bits {
    s: Vec<u8>,
    pos: usize,
    end: usize,
}

impl Bits {
    fn new(data: &[u8]) -> Bits {
        let mut s = Vec::with_capacity(data.len() + 128);
        let mut p = 0;
        while p < data.len() {
            let chunk = &data[p..(p + 65536).min(data.len())];
            for w in chunk[..chunk.len() & !3].chunks_exact(4) {
                s.extend_from_slice(&[w[3], w[2], w[1], w[0]]);
            }
            p += 65536 + 4;
        }
        let end = s.len() * 8;
        s.extend_from_slice(&[0u8; 128]);
        Bits { s, pos: 0, end }
    }

    fn peek(&self, n: u32) -> u32 {
        let b = self.pos >> 3;
        let v = u64::from_be_bytes(self.s[b..b + 8].try_into().unwrap());
        ((v >> (64 - (self.pos & 7) as u32 - n)) & ((1u64 << n) - 1)) as u32
    }

    fn skip(&mut self, n: u32) -> Result<(), String> {
        self.pos += n as usize;
        if self.pos > self.end + 64 { Err("data ended".into()) } else { Ok(()) }
    }

    fn read(&mut self, n: u32) -> Result<u32, String> {
        let v = self.peek(n);
        self.skip(n)?;
        Ok(v)
    }
}

/// The fixed code of the tree descriptions: (lowest first byte, symbol, bits), first bytes falling.
const DICT_SHORT: [(u32, u32, u32); 32] = [
    (0xE0, 0x08, 3), (0xC0, 0x09, 3), (0xA0, 0x0A, 3), (0x90, 0x00, 4), (0x80, 0x07, 4), (0x70, 0x0B, 4),
    (0x60, 0x0C, 4), (0x58, 0x06, 5), (0x50, 0x29, 5), (0x48, 0x2A, 5), (0x40, 0xE0, 5), (0x3C, 0x04, 6),
    (0x38, 0x05, 6), (0x34, 0x20, 6), (0x30, 0x28, 6), (0x2C, 0x2B, 6), (0x28, 0x2C, 6), (0x24, 0x40, 6),
    (0x20, 0x4A, 6), (0x1E, 0x03, 7), (0x1C, 0x0D, 7), (0x1A, 0x25, 7), (0x18, 0x26, 7), (0x16, 0x27, 7),
    (0x14, 0x48, 7), (0x12, 0x49, 7), (0x11, 0x24, 8), (0x10, 0x47, 8), (0x0F, 0x4B, 8), (0x0E, 0x4C, 8),
    (0x0D, 0x69, 8), (0x0C, 0x6A, 8),
];

struct Dict {
    hash: [Option<(u32, u32)>; 256],
    long: Vec<(u32, u32, Vec<u32>)>,
}

impl Dict {
    fn new() -> Dict {
        let mut hash = [None; 256];
        for (i, &(lo, sym, nb)) in DICT_SHORT.iter().enumerate() {
            let hi = if i == 0 { 0x100 } else { DICT_SHORT[i - 1].0 };
            for h in lo..hi {
                hash[h as usize] = Some((sym, nb));
            }
        }
        let range = |a: u32, b: u32| (a..b).collect::<Vec<_>>();
        let mut l16 = range(0x12, 0x20);
        l16.push(0x21);
        l16.extend(range(0x2F, 0x40));
        l16.extend([0x41, 0x42]);
        l16.extend(range(0x4F, 0x60));
        l16.extend([0x61, 0x62]);
        l16.extend(range(0x6E, 0x80));
        l16.extend([0x81, 0x82]);
        l16.extend(range(0x8E, 0xA0));
        l16.extend([0xA1, 0xA2, 0xA3]);
        l16.extend(range(0xAD, 0xC0));
        l16.extend([0xC1, 0xC2, 0xC3, 0xC5, 0xCB]);
        l16.extend(range(0xCD, 0xE0));
        l16.extend([0xE1, 0xE2, 0xE3, 0xEB]);
        l16.extend(range(0xED, 0x100));
        debug_assert_eq!(l16.len(), 160);
        let long = vec![
            (0x0700_0000, 9, vec![0x23, 0x46, 0x60, 0x63, 0x67, 0x68, 0x88, 0x89, 0xA0, 0xE8]),
            (0x0300_0000, 10, vec![0x01, 0x02, 0x2D, 0x43, 0x44, 0x45, 0x65, 0x66, 0x80, 0x87, 0x8A, 0xA8, 0xA9, 0xC0, 0xC9, 0xE9]),
            (0x0160_0000, 11, vec![0x0E, 0x4D, 0x64, 0x6B, 0x6C, 0x84, 0x85, 0x8B, 0xA4, 0xA5, 0xAA, 0xC8, 0xE5]),
            (0x00F0_0000, 12, vec![0x83, 0x86, 0xA6, 0xA7, 0xC7, 0xCA, 0xE7]),
            (0x00C0_0000, 13, vec![0x22, 0x2E, 0x8C, 0xC4, 0xE4, 0xE6]),
            (0x00B0_0000, 14, vec![0x4E, 0x6D, 0xC6, 0xEC]),
            (0x00A0_0000, 15, vec![0x0F, 0x10, 0x11, 0x8D, 0xAB, 0xAC, 0xCC, 0xEA]),
            (0x0000_0000, 16, l16),
        ];
        Dict { hash, long }
    }

    fn code(&self, bits: &mut Bits) -> Result<u32, String> {
        if let Some((sym, nb)) = self.hash[bits.peek(8) as usize] {
            bits.skip(nb)?;
            return Ok(sym);
        }
        let p = bits.peek(32);
        for (lowest, nb, syms) in &self.long {
            if p >= *lowest {
                let idx = ((p - lowest) >> (32 - nb)) as usize;
                if idx >= syms.len() {
                    return Err("bad dictionary code".into());
                }
                bits.skip(*nb)?;
                return Ok(syms[syms.len() - 1 - idx]);
            }
        }
        Err("dictionary code not found".into())
    }
}

/// A canonical Huffman tree, codes given from the longest-ones side.
#[derive(Default)]
struct Tree {
    single: Option<u32>,
    hash: Vec<Option<(u32, u32)>>,
    /// (lowest 32-bit value, bits, last index in `syms`), by growing length.
    long: Vec<(u32, u32, usize)>,
    syms: Vec<u32>,
}

impl Tree {
    fn parse(dict: &Dict, bits: &mut Bits, max_sym: u32) -> Result<Tree, String> {
        let num = bits.read(16)?;
        if num > max_sym + 1 {
            return Err("too many symbols".into());
        }
        let mut lengths = vec![0u32; num as usize];
        let mut remaining = num as i64 - 1;
        let single = (remaining & 0xFFFF) as u32;
        while remaining >= 0 {
            let code = dict.code(bits)?;
            let nb = code & 0x1F;
            let mut count = (code >> 5) + 1;
            if nb == 0 {
                remaining -= count as i64;
            } else {
                while count > 0 && remaining >= 0 {
                    lengths[remaining as usize] = nb;
                    remaining -= 1;
                    count -= 1;
                }
            }
        }
        let mut tree = Tree { hash: vec![None; 256], ..Tree::default() };
        let Some(&max_len) = lengths.iter().max().filter(|&&m| m > 0) else {
            tree.single = Some(single);
            return Ok(tree);
        };
        let mut by_len = vec![Vec::new(); max_len as usize + 1];
        for (sym, &l) in lengths.iter().enumerate() {
            if l > 0 {
                by_len[l as usize].push(sym as u32);
            }
        }
        let mut acode: i64 = 0;
        for nb in 0..=max_len {
            if nb <= 8 {
                for &sym in &by_len[nb as usize] {
                    let shift = 8 - nb;
                    let (from, to) = (acode << shift, ((acode + 1) << shift).min(256));
                    for h in from.max(0)..to {
                        tree.hash[h as usize] = Some((sym, nb));
                    }
                    acode -= 1;
                }
            } else if !by_len[nb as usize].is_empty() {
                for &sym in &by_len[nb as usize] {
                    tree.syms.push(sym);
                    acode -= 1;
                }
                tree.long.push(((((acode + 1) << (32 - nb)) & 0xFFFF_FFFF) as u32, nb, tree.syms.len() - 1));
            }
            acode = (acode << 1) + 1;
        }
        Ok(tree)
    }

    fn code(&self, bits: &mut Bits) -> Result<u32, String> {
        if let Some(sym) = self.single {
            return Ok(sym);
        }
        if let Some((sym, nb)) = self.hash[bits.peek(8) as usize] {
            bits.skip(nb)?;
            return Ok(sym);
        }
        let p = bits.peek(32);
        for &(lowest, nb, last) in &self.long {
            if p >= lowest {
                let idx = ((p - lowest) >> (32 - nb)) as usize;
                if idx > last {
                    return Err("bad Huffman code".into());
                }
                bits.skip(nb)?;
                return Ok(self.syms[last - idx]);
            }
        }
        Err("Huffman code not found".into())
    }
}

/// The archive's own LZ77 + Huffman. With `limit`, stops after that many bytes.
pub fn decompress(data: &[u8], limit: Option<usize>) -> Result<Vec<u8>, String> {
    thread_local!(static DICT: Dict = Dict::new());
    if data.len() < 12 {
        return Err("file too short".into());
    }
    DICT.with(|dict| {
        let mut bits = Bits::new(data);
        bits.read(32)?;
        let full = bits.read(32)? as usize;
        if full == 0 || full > 256 * 1024 * 1024 {
            return Err("implausible size".into());
        }
        bits.read(4)?;
        let size_add = bits.read(4)? as usize + 1;
        let out_size = limit.map_or(full, |l| l.min(full));
        let mut out = vec![0u8; out_size];
        let mut pos = 0;
        while pos < out_size {
            let symbols = Tree::parse(dict, &mut bits, 284)?;
            let copies = Tree::parse(dict, &mut bits, 29)?;
            let max_codes = ((bits.read(4)? + 1) << 12) as usize;
            let mut n = 0;
            while n < max_codes && pos < out_size {
                let symbol = symbols.code(&mut bits)?;
                n += 1;
                if symbol < 0x100 {
                    out[pos] = symbol as u8;
                    pos += 1;
                    continue;
                }
                let sym = symbol - 0x100;
                let (div4, rem4) = (sym / 4, sym % 4);
                let mut size = if div4 == 0 {
                    sym
                } else if div4 < 7 {
                    (1 << (div4 - 1)) * (4 + rem4)
                } else if sym == 28 {
                    0xFF
                } else {
                    return Err("bad length".into());
                };
                if div4 > 1 && sym != 28 {
                    size |= bits.read(div4 - 1)?;
                }
                let size = size as usize + size_add;
                let off_sym = copies.code(&mut bits)?;
                let (div2, rem2) = (off_sym / 2, off_sym % 2);
                let mut offset = if div2 == 0 {
                    off_sym
                } else if div2 < 17 {
                    (1 << (div2 - 1)) * (2 + rem2)
                } else {
                    return Err("bad distance".into());
                };
                if div2 > 1 {
                    offset |= bits.read(div2 - 1)?;
                }
                let offset = offset as usize + 1;
                if pos < offset {
                    return Err("distance before the start".into());
                }
                for _ in 0..size {
                    if pos >= out_size {
                        break;
                    }
                    out[pos] = out[pos - offset];
                    pos += 1;
                }
            }
        }
        Ok(out)
    })
}

// ---------------------------------------------------------------- strs files and texts

pub struct StrsEntry {
    pub dec_off: u16,
    pub bps: u16,
    pub data: Vec<u8>,
}

/// The texts of a strs file and its tail (language, block modulo 255).
pub fn parse_strs(d: &[u8]) -> Option<(Vec<StrsEntry>, Vec<u8>)> {
    if d.get(..4)? != b"strs" {
        return None;
    }
    let (mut pos, mut out) = (4, Vec::new());
    while pos + 6 <= d.len() {
        let (size, dec_off, bps) = (u16_at(d, pos) as usize, u16_at(d, pos + 2), u16_at(d, pos + 4));
        if size < 6 || pos + size > d.len() {
            break;
        }
        out.push(StrsEntry { dec_off, bps, data: d[pos + 6..pos + size].to_vec() });
        pos += size;
    }
    Some((out, d[pos..].to_vec()))
}

/// The 20-byte RC4 key of a text, from its seed.
pub fn key20(seed: u64) -> [u8; 20] {
    let s = seed.to_le_bytes();
    let w = |i: usize| u32::from_le_bytes([s[(i * 4) & 7], s[(i * 4 + 1) & 7], s[(i * 4 + 2) & 7], s[(i * 4 + 3) & 7]]);
    let (a, b, c, d, e) = (w(0), w(1), w(2), w(3), w(4));
    let mut r15 = a.wrapping_sub(0x604B_674D);
    let eax = r15.rotate_left(30);
    let mut r10 = r15.rotate_left(5).wrapping_add(b).wrapping_add(0x66B0_CD0D);
    let mut esi = r10;
    let mut ebp: u32 = 0x59D1_48C0;
    let mut r11 = (eax ^ ebp) & r10;
    r10 = r10.rotate_left(30);
    r15 = (r15 & 0x2222_2222) ^ 0x7BF3_6AE2;
    esi = esi.rotate_left(5);
    r15 = r15.wrapping_add(c);
    esi = esi.wrapping_add(r15).wrapping_add(0xF33D_5697);
    r15 = esi;
    let mut r12 = (r10 ^ eax) & esi;
    esi = esi.rotate_left(30);
    r11 ^= ebp;
    r15 = r15.rotate_left(5);
    r11 = r11.wrapping_add(d);
    ebp = r11;
    r11 = r11.wrapping_add(r15);
    r15 = r15.wrapping_add(ebp).wrapping_add(0xD675_E47B);
    r12 ^= eax;
    r15 = r15.rotate_left(5);
    let r9 = a.wrapping_add(e).wrapping_add(r12).wrapping_add(r15).wrapping_add(0xB453_C259);
    let r8 = b.wrapping_add(r11).wrapping_add(0xD675_E47B);
    let mut key = [0u8; 20];
    for (i, v) in [r9, r8, esi.wrapping_add(c), r10.wrapping_add(d), eax.wrapping_add(e)].iter().enumerate() {
        key[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    key
}

pub fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut j: u8 = 0;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let (mut i, mut j) = (0u8, 0u8);
    data.iter()
        .map(|b| {
            i = i.wrapping_add(1);
            j = j.wrapping_add(s[i as usize]);
            s.swap(i as usize, j as usize);
            b ^ s[s[i as usize].wrapping_add(s[j as usize]) as usize]
        })
        .collect()
}

/// The characters of the symbols below 0x20.
const SMALL: &[u8] = b"0123456strnum()[]<>%#/:-'\" ,.!\n";

/// An encrypted text opened with its seed; None when the seed does not fit it (with the right
/// seed the symbols fill the text and only zero padding follows).
pub fn decode(data: &[u8], dec_off: u16, bps: u16, seed: u64) -> Option<String> {
    if !(1..=14).contains(&bps) {
        return None;
    }
    let p = rc4(&key20(seed), data);
    let bit = |i: usize| (p[i / 8] >> (i % 8)) & 1;
    let bps = bps as usize;
    let total = p.len() * 8;
    let nsym = total / bps;
    let any_from = |from: usize| (from..total).any(|i| bit(i) == 1);
    let mut out = String::new();
    let mut count = 0;
    for k in 0..nsym {
        let s = (0..bps).fold(0u32, |v, j| v | (bit(k * bps + j) as u32) << j);
        if s == 0 {
            if any_from(k * bps) {
                return None;
            }
            break;
        }
        count += 1;
        if s < 0x20 {
            out.push(SMALL[s as usize - 1] as char);
        } else {
            let c = s + dec_off as u32 - 0x20;
            if (0x7F..0xA0).contains(&c) || (0xD800..0xE000).contains(&c) || c >= 0xFFFE {
                return None;
            }
            out.push(char::from_u32(c)?);
        }
    }
    if (count == nsym && any_from(nsym * bps)) || count + 2 < nsym || count == 0 {
        return None;
    }
    Some(out)
}

/// A plain text (16 bits per character: UTF-16).
pub fn plain(data: &[u8]) -> String {
    let units: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    String::from_utf16_lossy(&units).trim_end_matches('\0').to_owned()
}

/// The characters an English text of the game has beyond Latin: dashes, quotes, bullets, ellipsis.
const TYPOGRAPHIC: &[char] = &['\u{2013}', '\u{2014}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2026}', '\u{2122}'];

/// A text worth translating: some letter in it, and English-looking. A seed that does not belong
/// to the text can still pass decode()'s padding check by chance: what it opens is then a run of
/// CJK and other far characters ("⌚⻼ಞ㑰˸…"), which an English text never is.
fn worth(en: &str) -> bool {
    // The developers' placeholders of texts never filled (about 24,000 in 2026).
    if ["[null]", "(new string)"].contains(&en.trim()) {
        return false;
    }
    let far = en.chars().filter(|c| *c as u32 > 0x2FF && !TYPOGRAPHIC.contains(c)).count();
    en.chars().any(char::is_alphabetic) && far * 20 <= en.chars().count() && far <= 3
}

// ---------------------------------------------------------------- the seed pool

/// Text Translator's shared seed pool (its author allows reading it; we only read: our list of new
/// seeds is always empty). Request: bitcode of (last sequence we have, our new seeds); answer:
/// bitcode of (seeds since then as (text ID, seed), more pages, the next sequence).
pub const SEED_POOL: &str = "https://lang-server-production.up.railway.app/sync_seeds";

const SEEDS_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS seeds (id INTEGER PRIMARY KEY NOT NULL, synced INTEGER NOT NULL CHECK (synced IN (0, 1)), seed INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS sync_state (table_name TEXT PRIMARY KEY, last_seq INTEGER NOT NULL DEFAULT 0) WITHOUT ROWID;
";

pub fn request(last_seq: u64) -> Vec<u8> {
    bitcode::encode(&(last_seq, Vec::<(u32, u64)>::new()))
}

pub fn answer(body: &[u8]) -> Result<(Vec<(u32, u64)>, bool, u64), String> {
    bitcode::decode(body).map_err(|e| format!("seed pool answer: {e}"))
}

/// Brings `file` (Text Translator's seeds.db layout) up to date with the pool: (new seeds, total).
pub fn sync_seeds(file: &str) -> Result<(usize, usize), String> {
    let con = Connection::open(file).map_err(|e| format!("{file}: {e}"))?;
    con.execute_batch(SEEDS_SCHEMA).map_err(|e| e.to_string())?;
    let mut last: u64 = con
        .query_row("SELECT last_seq FROM sync_state WHERE table_name = 'seeds'", [], |r| r.get::<_, i64>(0))
        .optional()
        .map_err(|e| e.to_string())?
        .unwrap_or(0) as u64;
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(300))).build().into();
    let mut new = 0;
    for _page in 0..1000 {
        let mut resp = agent
            .post(SEED_POOL)
            .header("Content-Type", "application/octet-stream")
            .send(&request(last)[..])
            .map_err(|e| format!("seed pool: {e}"))?;
        let body = resp.body_mut().with_config().limit(256 << 20).read_to_vec().map_err(|e| format!("seed pool: {e}"))?;
        let (seeds, more, next) = answer(&body)?;
        let tx = con.unchecked_transaction().map_err(|e| e.to_string())?;
        {
            let mut put = tx.prepare("INSERT OR REPLACE INTO seeds (id, synced, seed) VALUES (?1, 1, ?2)").map_err(|e| e.to_string())?;
            for (id, seed) in &seeds {
                new += put.execute(params![id, *seed as i64]).map_err(|e| e.to_string())?;
            }
            tx.execute("INSERT OR REPLACE INTO sync_state (table_name, last_seq) VALUES ('seeds', ?1)", [next as i64]).map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        if !more || next <= last {
            break;
        }
        last = next;
    }
    let total: i64 = con.query_row("SELECT count(*) FROM seeds", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    Ok((new, total as usize))
}

// ---------------------------------------------------------------- the run

#[derive(Default, Debug)]
pub struct Report {
    pub scanned: usize,
    pub rescanned: usize,
    pub english_files: usize,
    pub with_block: usize,
    pub plain: usize,
    pub seeded: usize,
    pub no_seed: usize,
    pub bad_seed: usize,
    pub empty: usize,
}

const OUT_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS texts (key INTEGER PRIMARY KEY, en TEXT NOT NULL, how TEXT NOT NULL, file INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS files (file INTEGER PRIMARY KEY, idx INTEGER NOT NULL, block INTEGER, entries INTEGER NOT NULL,
    plain INTEGER NOT NULL, seeded INTEGER NOT NULL, matched INTEGER NOT NULL);
-- The scan of the archive, to read again only what changed: english = 1 for an English strs file.
CREATE TABLE IF NOT EXISTS scan (idx INTEGER PRIMARY KEY, offset INTEGER NOT NULL, size INTEGER NOT NULL, crc INTEGER NOT NULL,
    english INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
";

/// IDs -> English we already know (our database, the map), for placing the files.
fn known(db: Option<&str>, map: Option<&str>) -> Result<HashMap<u32, HashSet<String>>, String> {
    let mut known: HashMap<u32, HashSet<String>> = HashMap::new();
    for (file, sql) in [(db, "SELECT id, en FROM translations"), (map, "SELECT key, en FROM texts")] {
        let Some(file) = file else { continue };
        let con = Connection::open_with_flags(file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| format!("{file}: {e}"))?;
        let mut stmt = con.prepare(sql).map_err(|e| format!("{file}: {e}"))?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).map_err(|e| format!("{file}: {e}"))?;
        for (id, en) in rows.flatten() {
            known.entry(id as u32).or_default().insert(en);
        }
    }
    Ok(known)
}

fn seeds(file: &str) -> Result<HashMap<u32, u64>, String> {
    let con = Connection::open_with_flags(file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| format!("{file}: {e}"))?;
    let mut stmt = con.prepare("SELECT id, seed FROM seeds").map_err(|e| format!("{file}: {e}"))?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)? as u32, r.get::<_, i64>(1)? as u64))).map_err(|e| format!("{file}: {e}"))?;
    Ok(rows.flatten().collect())
}

/// How well a file sits in a block: (texts equal to the known ones, seeds that open a text, seeds tried).
fn evidence(entries: &[StrsEntry], block: u32, seeds: &HashMap<u32, u64>, known: &HashMap<u32, HashSet<String>>) -> (usize, usize, usize) {
    let (mut matched, mut opened, mut tried) = (0, 0, 0);
    for (i, e) in entries.iter().enumerate() {
        if e.data.is_empty() {
            continue;
        }
        let key = block * 1024 + i as u32;
        if e.bps == 16 {
            if known.get(&key).is_some_and(|k| k.contains(&plain(&e.data))) {
                matched += 1;
            }
            continue;
        }
        let Some(&seed) = seeds.get(&key) else { continue };
        if tried >= SEEDS_TRIED {
            continue;
        }
        tried += 1;
        if let Some(text) = decode(&e.data, e.dec_off, e.bps, seed) {
            opened += 1;
            if known.get(&key).is_some_and(|k| k.contains(&text)) {
                matched += 1;
            }
        }
    }
    (matched, opened, tried)
}

pub fn run(dat_path: &str, seeds_path: &str, out_path: &str, db: Option<&str>, map: Option<&str>) -> Result<Report, String> {
    let mut report = Report::default();
    let mut dat = Dat::open(dat_path)?;
    let seeds = seeds(seeds_path)?;
    let known = known(db, map)?;
    let tmp = format!("{out_path}.tmp");
    let _ = std::fs::remove_file(&tmp);
    let out = Connection::open(&tmp).map_err(|e| format!("{tmp}: {e}"))?;
    out.execute_batch(OUT_SCHEMA).map_err(|e| e.to_string())?;

    // The previous scan: unchanged entries are not read again.
    let mut previous: HashMap<u32, (Entry, bool)> = HashMap::new();
    if std::path::Path::new(out_path).exists() {
        let old = Connection::open_with_flags(out_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
        let has = old.query_row("SELECT 1 FROM sqlite_master WHERE name = 'scan'", [], |_| Ok(())).optional().map_err(|e| e.to_string())?;
        if has.is_some() {
            let mut stmt = old.prepare("SELECT idx, offset, size, crc, english FROM scan").map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((r.get::<_, i64>(0)? as u32, r.get::<_, i64>(1)? as u64, r.get::<_, i64>(2)? as u32, r.get::<_, i64>(3)? as u32, r.get::<_, bool>(4)?))
                })
                .map_err(|e| e.to_string())?;
            for (idx, offset, size, crc, english) in rows.flatten() {
                previous.insert(idx, (Entry { offset, size, compressed: false, crc }, english));
            }
        }
    }

    // 1. The English strs files: (current file ID, MFT index, texts, tail).
    let mut indices: Vec<(u32, u32)> = dat.top.iter().filter(|(i, _)| **i > 2).map(|(i, f)| (*i, *f)).collect();
    indices.sort();
    let mut files = Vec::new();
    let tx = out.unchecked_transaction().map_err(|e| e.to_string())?;
    {
        let mut put = tx.prepare("INSERT OR REPLACE INTO scan (idx, offset, size, crc, english) VALUES (?1, ?2, ?3, ?4, ?5)").map_err(|e| e.to_string())?;
        for &(idx, file_id) in &indices {
            let Some(e) = dat.entry(idx) else { continue };
            report.scanned += 1;
            let same = previous.get(&idx).filter(|(p, _)| p.offset == e.offset && p.size == e.size && p.crc == e.crc).map(|(_, english)| *english);
            let english = match same {
                Some(false) => false,
                Some(true) | None => {
                    if same.is_none() {
                        report.rescanned += 1;
                    }
                    let candidate = (16..=STRS_MAX).contains(&e.size) && (same == Some(true) || dat.read_raw(e, Some(4)).is_ok_and(|b| b == b"strs"));
                    match candidate.then(|| dat.read_raw(e, None)).and_then(Result::ok).and_then(|d| parse_strs(&d)) {
                        Some((entries, tail)) if tail.len() == 2 && tail[0] == 0 => {
                            files.push((file_id, idx, entries, tail[1]));
                            true
                        }
                        _ => false,
                    }
                }
            };
            put.execute(params![idx, e.offset as i64, e.size, e.crc, english]).map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    report.english_files = files.len();

    // 2. The block of each file: of those that fit its last byte, the one the evidence points to.
    let max_block = seeds.keys().chain(known.keys()).max().map_or(0, |k| k >> 10) + 64;
    let mut proposals = Vec::new();
    for (n, (_, _, entries, low)) in files.iter().enumerate() {
        for block in (*low as u32..=max_block).step_by(255) {
            let (matched, opened, tried) = evidence(entries, block, &seeds, &known);
            if matched >= 3 {
                proposals.push((2, matched, opened, n, block));
            } else if tried >= 3 && opened >= 3 && opened * 10 >= tried * 9 {
                proposals.push((1, matched, opened, n, block));
            }
        }
    }
    proposals.sort_by(|a, b| b.cmp(a));
    let (mut block_of, mut used) = (HashMap::new(), HashSet::new());
    for (_, _, _, n, block) in proposals {
        if !block_of.contains_key(&n) && used.insert(block) {
            block_of.insert(n, block);
        }
    }
    report.with_block = block_of.len();

    // 3. The texts.
    let tx = out.unchecked_transaction().map_err(|e| e.to_string())?;
    {
        let mut text = tx.prepare("INSERT OR REPLACE INTO texts (key, en, how, file) VALUES (?1, ?2, ?3, ?4)").map_err(|e| e.to_string())?;
        let mut file = tx
            .prepare("INSERT OR REPLACE INTO files (file, idx, block, entries, plain, seeded, matched) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)")
            .map_err(|e| e.to_string())?;
        for (n, (file_id, idx, entries, _)) in files.iter().enumerate() {
            let block = block_of.get(&n).copied();
            let (mut plain_n, mut seeded_n, mut matched) = (0, 0, 0);
            if let Some(block) = block {
                for (i, e) in entries.iter().enumerate() {
                    if e.data.is_empty() {
                        report.empty += 1;
                        continue;
                    }
                    let key = block * 1024 + i as u32;
                    let (en, how) = if e.bps == 16 {
                        (plain(&e.data), "plain")
                    } else if let Some(&seed) = seeds.get(&key) {
                        match decode(&e.data, e.dec_off, e.bps, seed) {
                            Some(t) => (t, "seed"),
                            None => {
                                report.bad_seed += 1;
                                continue;
                            }
                        }
                    } else {
                        report.no_seed += 1;
                        continue;
                    };
                    if known.get(&key).is_some_and(|k| k.contains(&en)) {
                        matched += 1;
                    }
                    if !worth(&en) {
                        continue;
                    }
                    if how == "plain" {
                        plain_n += 1;
                    } else {
                        seeded_n += 1;
                    }
                    text.execute(params![key, en, how, file_id]).map_err(|e| e.to_string())?;
                }
            }
            report.plain += plain_n;
            report.seeded += seeded_n;
            file.execute(params![file_id, idx, block, entries.len(), plain_n, seeded_n, matched]).map_err(|e| e.to_string())?;
        }
        let mut meta = tx.prepare("INSERT OR REPLACE INTO meta (k, v) VALUES (?1, ?2)").map_err(|e| e.to_string())?;
        meta.execute(params!["run", format!("{} {:?}", crate::now_ms(), report)]).map_err(|e| e.to_string())?;
        meta.execute(params!["seeds", seeds.len().to_string()]).map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    drop(out);
    std::fs::rename(&tmp, out_path).map_err(|e| format!("{out_path}: {e}"))?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rc4_known_vector() {
        // RFC 6229 style check: key "Key", plaintext "Plaintext" -> BBF316E8D940AF0AD3
        let out = rc4(b"Key", b"Plaintext");
        assert_eq!(out, [0xBB, 0xF3, 0x16, 0xE8, 0xD9, 0x40, 0xAF, 0x0A, 0xD3]);
    }

    #[test]
    fn a_text_round_trips_through_its_seed() {
        // Pack "Hello" with 7 bits a symbol (dec_off 0x20: symbol = char), zero padding, encrypt.
        let (dec_off, bps, seed) = (0x20u16, 7u16, 23_558_860_849_292u64);
        let mut bits: Vec<u8> = Vec::new();
        for c in "Hello".chars() {
            for j in 0..7 {
                bits.push(((c as u32 >> j) & 1) as u8);
            }
        }
        while bits.len() % 8 != 0 {
            bits.push(0);
        }
        let packed: Vec<u8> = bits.chunks(8).map(|b| b.iter().enumerate().fold(0, |v, (i, x)| v | (x << i))).collect();
        let data = rc4(&key20(seed), &packed);
        assert_eq!(decode(&data, dec_off, bps, seed).as_deref(), Some("Hello"));
        assert_eq!(decode(&data, dec_off, bps, seed + 1), None);
    }

    #[test]
    fn a_wrong_seed_s_noise_is_not_a_text() {
        assert!(worth("Knocks Down • Throws Grubs • Inflicts Vulnerability"));
        assert!(worth("“I carry the wind.” — Archers of the Dominion…"));
        assert!(!worth("⌚\u{2efc}ಞ㑰˸\u{2e75}㳢✊རᡉᴻŔ㑂㌳՟㵇ॷ㦅᜵㬩㞦ḃ⪐űᓽ⼊⢕"));
        assert!(!worth("[null]") && !worth("(new string)"));
    }

    #[test]
    fn the_seed_pool_s_messages() {
        // The request Text Translator sends: (last sequence, its new seeds) - ours has none.
        assert_eq!(request(419_000), vec![2, 184, 100, 6, 0, 0]);
        let body = bitcode::encode(&(vec![(1_069_884u32, 30_645_249_454_306u64)], false, 430_757u64));
        assert_eq!(answer(&body).unwrap(), (vec![(1_069_884, 30_645_249_454_306)], false, 430_757));
        assert!(answer(&[1, 2, 3]).is_err());
    }

    #[test]
    fn plain_texts_are_utf16() {
        let data: Vec<u8> = "Rawhide".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(plain(&data), "Rawhide");
    }

    #[test]
    fn strs_tail_is_language_and_block() {
        let mut d = b"strs".to_vec();
        let text: Vec<u8> = "Hi".encode_utf16().flat_map(u16::to_le_bytes).collect();
        d.extend_from_slice(&((6 + text.len()) as u16).to_le_bytes());
        d.extend_from_slice(&0u16.to_le_bytes());
        d.extend_from_slice(&16u16.to_le_bytes());
        d.extend_from_slice(&text);
        d.extend_from_slice(&[0, 42]);
        let (entries, tail) = parse_strs(&d).unwrap();
        assert_eq!((entries.len(), plain(&entries[0].data), tail), (1, "Hi".to_owned(), vec![0, 42]));
    }
}

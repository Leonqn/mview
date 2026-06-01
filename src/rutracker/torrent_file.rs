//! Minimal bencode reader for extracting metadata from a downloaded `.torrent`
//! file. We only need the BitTorrent v1 info-hash (the hash qBittorrent keys
//! torrents by) and the `name` field, so this is a tiny offset-tracking scanner
//! rather than a full bencode deserializer.
//!
//! Why this exists: rutracker's topic page parsing is flaky and can return an
//! empty hash. The `.torrent` bytes are the authoritative source, so deriving
//! the hash here keeps the qBittorrent torrent and the DB row linkable even
//! when the topic page comes back empty.

use sha1::{Digest, Sha1};

/// Parse raw `.torrent` bytes into `(info_hash_hex, name)`.
///
/// `info_hash_hex` is the lowercase hex SHA-1 of the bencoded `info` dict — the
/// v1 info-hash qBittorrent uses. `name` is the torrent's `info.name` field, a
/// good title fallback. Returns `None` if the bytes aren't a parseable metainfo
/// dict (e.g. rutracker served HTML instead of a torrent).
pub fn parse_torrent_meta(bytes: &[u8]) -> Option<(String, Option<String>)> {
    let mut scanner = Scanner {
        data: bytes,
        pos: 0,
    };
    let entries = scanner.dict()?;
    let (info_start, info_end) = entries
        .iter()
        .find(|(key, _)| *key == b"info")
        .map(|(_, span)| *span)?;
    let info_bytes = bytes.get(info_start..info_end)?;

    let mut hasher = Sha1::new();
    hasher.update(info_bytes);
    let info_hash: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    let name = Scanner {
        data: info_bytes,
        pos: 0,
    }
    .dict()
    .and_then(|info_entries| {
        info_entries
            .into_iter()
            .find(|(key, _)| *key == b"name")
            .and_then(|(_, (start, end))| {
                Scanner {
                    data: info_bytes.get(start..end)?,
                    pos: 0,
                }
                .read_string()
                .and_then(|raw| std::str::from_utf8(raw).ok())
                .map(|s| s.to_string())
            })
    });

    Some((info_hash, name))
}

/// A dict entry: the key bytes paired with the `(start, end)` byte span of its
/// value in the source buffer.
type DictEntry<'a> = (&'a [u8], (usize, usize));

/// Offset-tracking bencode scanner. Advances `pos` over elements and records
/// raw byte spans so the `info` dict can be hashed without re-encoding.
struct Scanner<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Scanner<'a> {
    fn byte(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    /// Read a bencoded string length prefix (`<digits>:`), leaving `pos` at the
    /// first content byte.
    fn read_strlen(&mut self) -> Option<usize> {
        let mut len = 0usize;
        loop {
            let c = self.byte()?;
            if c == b':' {
                self.pos += 1;
                return Some(len);
            }
            if !c.is_ascii_digit() {
                return None;
            }
            len = len.checked_mul(10)?.checked_add((c - b'0') as usize)?;
            self.pos += 1;
        }
    }

    /// Read a bencoded string element, returning its content bytes.
    fn read_string(&mut self) -> Option<&'a [u8]> {
        let len = self.read_strlen()?;
        let content = self.data.get(self.pos..self.pos.checked_add(len)?)?;
        self.pos += len;
        Some(content)
    }

    /// Advance past one bencoded element, returning its `(start, end)` byte span.
    fn skip(&mut self) -> Option<(usize, usize)> {
        let start = self.pos;
        match self.byte()? {
            b'i' => {
                self.pos += 1;
                while self.byte()? != b'e' {
                    self.pos += 1;
                }
                self.pos += 1;
            }
            b'l' | b'd' => {
                self.pos += 1;
                while self.byte()? != b'e' {
                    self.skip()?;
                }
                self.pos += 1;
            }
            b'0'..=b'9' => {
                let len = self.read_strlen()?;
                self.pos = self.pos.checked_add(len)?;
                if self.pos > self.data.len() {
                    return None;
                }
            }
            _ => return None,
        }
        Some((start, self.pos))
    }

    /// Parse a dict element, returning `(key, value_span)` pairs.
    fn dict(&mut self) -> Option<Vec<DictEntry<'a>>> {
        if self.byte()? != b'd' {
            return None;
        }
        self.pos += 1;
        let mut entries = Vec::new();
        loop {
            if self.byte()? == b'e' {
                self.pos += 1;
                return Some(entries);
            }
            let key = self.read_string()?;
            let span = self.skip()?;
            entries.push((key, span));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_info_hash_and_name() {
        // Minimal valid metainfo: d 4:info d 6:length i1e 4:name 5:hello e e
        let torrent = b"d4:infod6:lengthi1e4:name5:helloee";
        let (hash, name) = parse_torrent_meta(torrent).expect("should parse");
        assert_eq!(name.as_deref(), Some("hello"));
        // SHA-1 of the info dict bytes "d6:lengthi1e4:name5:helloe"
        let mut hasher = Sha1::new();
        hasher.update(b"d6:lengthi1e4:name5:helloe");
        let expected: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(hash, expected);
        assert_eq!(hash.len(), 40);
    }

    #[test]
    fn rejects_non_torrent_html() {
        assert!(parse_torrent_meta(b"<!DOCTYPE html><html>nope</html>").is_none());
    }

    #[test]
    fn rejects_empty() {
        assert!(parse_torrent_meta(b"").is_none());
    }
}

//! SHA-256 (NIST FIPS 180-4), for verifying corpus downloads without depending on `sha256sum`, `shasum` or a crate.
//!
//! Adapted from PhotoCraft's `xtask/src/sha256.rs` (MIT OR Apache-2.0; see ATTRIBUTION.md and NOTICE), made
//! incremental so that a large file is hashed as it is read rather than copied into memory first.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

#[allow(clippy::unreadable_literal)] // The round constants as FIPS 180-4 prints them.
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
    0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

#[allow(clippy::unreadable_literal)] // The initial hash value as FIPS 180-4 prints it.
const INITIAL: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];

/// An incremental SHA-256 hasher.
pub struct Sha256 {
    state: [u32; 8],
    /// Bytes waiting for a whole 64-byte block.
    block: [u8; 64],
    buffered: usize,
    /// Message length in bytes.
    length: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Sha256 { state: INITIAL, block: [0; 64], buffered: 0, length: 0 }
    }
}

impl Sha256 {
    pub fn update(&mut self, mut data: &[u8]) {
        self.length = self.length.wrapping_add(data.len() as u64);
        if self.buffered > 0 {
            let take = (64 - self.buffered).min(data.len());
            let (head, rest) = data.split_at(take);
            self.block[self.buffered..self.buffered + take].copy_from_slice(head);
            self.buffered += take;
            data = rest;
            if self.buffered < 64 {
                return;
            }
            compress(&mut self.state, &self.block);
            self.buffered = 0;
        }
        let (blocks, rest) = data.as_chunks::<64>();
        for block in blocks {
            compress(&mut self.state, block);
        }
        self.block[..rest.len()].copy_from_slice(rest);
        self.buffered = rest.len();
    }

    /// The digest, after the padding of FIPS 180-4 section 5.1.1.
    pub fn finish(mut self) -> [u8; 32] {
        let bits = self.length.wrapping_mul(8);
        let zeros = (55 + 64 - self.buffered % 64) % 64;
        let mut padding = vec![0x80];
        padding.resize(1 + zeros, 0);
        padding.extend_from_slice(&bits.to_be_bytes());
        self.update(&padding);
        let mut digest = [0; 32];
        for (out, word) in digest.as_chunks_mut::<4>().0.iter_mut().zip(self.state) {
            *out = word.to_be_bytes();
        }
        digest
    }
}

fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (word, bytes) in w.iter_mut().zip(block.as_chunks::<4>().0) {
        *word = u32::from_be_bytes(*bytes);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for (k, w) in K.iter().zip(w) {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(*k).wrapping_add(w);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (x, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *x = x.wrapping_add(v);
    }
}

/// Lower-case hex of a digest.
pub fn hex(digest: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Lower-case hex SHA-256 of `data`.
#[cfg(test)]
pub fn hex_of(data: &[u8]) -> String {
    let mut hasher = Sha256::default();
    hasher.update(data);
    hex(&hasher.finish())
}

/// The lower-case hex SHA-256 of the file at `path` and its length in bytes, read in 64 KiB pieces.
pub fn file(path: &Path) -> io::Result<(String, u64)> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::default();
    let mut buffer = vec![0; 64 * 1024];
    let mut length = 0u64;
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                hasher.update(&buffer[..n]);
                length += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok((hex(&hasher.finish()), length))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        // FIPS 180-4 examples and the NIST million-a test.
        assert_eq!(hex_of(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex_of(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            hex_of(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(hex_of(&[b'a'; 1000]), "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3");
        assert_eq!(hex_of(&vec![b'a'; 1_000_000]), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
    }

    #[test]
    fn pieces_of_any_size_give_the_same_digest() {
        let data: Vec<u8> = (0..1000u32).map(|i| u8::try_from(i * 7 % 251).unwrap()).collect();
        let whole = hex_of(&data);
        for piece in [1, 3, 55, 56, 63, 64, 65, 128, 999] {
            let mut hasher = Sha256::default();
            for chunk in data.chunks(piece) {
                hasher.update(chunk);
            }
            assert_eq!(hex(&hasher.finish()), whole, "pieces of {piece}");
        }
        // Every padding length: messages of 0 to 130 bytes.
        for len in 0..130 {
            let mut hasher = Sha256::default();
            hasher.update(&data[..len]);
            assert_eq!(hex(&hasher.finish()), hex_of(&data[..len]));
        }
    }

    #[test]
    fn files_are_hashed_with_their_length() {
        let dir = std::env::temp_dir().join(format!("florastack-sha256-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("abc.txt");
        std::fs::write(&path, b"abc").unwrap();
        let hashed = file(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(hashed, ("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_owned(), 3));
    }
}

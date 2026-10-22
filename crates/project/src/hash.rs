//! [`ContentHash`]: the SHA-256 of a file's bytes (FIPS 180-4), written `sha256:` and 64 hexadecimal digits. A project
//! records one for every input frame, so that a changed or replaced file is noticed, and for every cache file, so
//! that a damaged one is never used. SHA-256 rather than a faster non-cryptographic hash because these hashes leave
//! FloraStack: the reproducibility sidecar of scientific exports (roadmap 6.2) lists the input hashes, and anyone can
//! check those with `sha256sum` or `Get-FileHash`.

use std::fmt;
use std::io::Read;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

/// A SHA-256 content hash.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentHash([u8; 32]);

/// Why a text isn't a content hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("not a content hash: expected `sha256:` and 64 hexadecimal digits")]
pub struct HashError;

const PREFIX: &str = "sha256:";

impl ContentHash {
    /// The hash of `bytes`.
    pub fn of(bytes: &[u8]) -> ContentHash {
        let mut h = Hasher::new();
        h.update(bytes);
        h.finish()
    }

    /// The hash of everything `reader` yields, and how many bytes that was.
    pub fn of_reader(mut reader: impl Read) -> std::io::Result<(ContentHash, u64)> {
        let mut h = Hasher::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => h.update(buf.get(..n).unwrap_or_default()),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        let len = h.len;
        Ok((h.finish(), len))
    }

    pub fn from_bytes(bytes: [u8; 32]) -> ContentHash {
        ContentHash(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The first 16 hexadecimal digits, which name cache files.
    pub fn short(&self) -> String {
        hex(self.0.get(..8).unwrap_or_default())
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{PREFIX}{}", hex(&self.0))
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for ContentHash {
    type Err = HashError;

    fn from_str(text: &str) -> Result<ContentHash, HashError> {
        let digits = text.strip_prefix(PREFIX).ok_or(HashError)?.as_bytes();
        if digits.len() != 64 {
            return Err(HashError);
        }
        let mut out = [0u8; 32];
        let (pairs, _) = digits.as_chunks::<2>();
        for (byte, &[hi, lo]) in out.iter_mut().zip(pairs) {
            let nibble = |c: u8| char::from(c).to_digit(16).and_then(|d| u8::try_from(d).ok());
            let (Some(hi), Some(lo)) = (nibble(hi), nibble(lo)) else {
                return Err(HashError);
            };
            *byte = hi << 4 | lo;
        }
        Ok(ContentHash(out))
    }
}

impl Serialize for ContentHash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<ContentHash, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(|e: HashError| serde::de::Error::custom(format!("{text:?} is {e}")))
    }
}

/// A hash being computed, with the number of bytes it has seen.
pub(crate) struct Hasher {
    inner: Sha256,
    pub(crate) len: u64,
}

impl Hasher {
    pub(crate) fn new() -> Hasher {
        Hasher { inner: Sha256::new(), len: 0 }
    }

    pub(crate) fn update(&mut self, bytes: &[u8]) {
        self.inner.update(bytes);
        self.len = self.len.saturating_add(bytes.len() as u64);
    }

    pub(crate) fn finish(self) -> ContentHash {
        ContentHash(self.inner.finalize().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_digests() {
        // FIPS 180-4's examples (NIST's "SHA256.pdf"), and the empty input.
        assert_eq!(ContentHash::of(b"abc").to_string(), "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(ContentHash::of(b"").to_string(), "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        let long = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        assert_eq!(ContentHash::of(long).to_string(), "sha256:248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1");
        let (streamed, len) = ContentHash::of_reader(&long[..]).unwrap();
        assert_eq!((streamed, len), (ContentHash::of(long), long.len() as u64));
        assert_eq!(ContentHash::of(b"abc").short(), "ba7816bf8f01cfea");
    }

    #[test]
    fn text_round_trips_and_rejects_malformed_hashes() {
        let h = ContentHash::of(b"frame");
        assert_eq!(h.to_string().parse::<ContentHash>(), Ok(h));
        assert_eq!(h.to_string().to_uppercase().replace("SHA256:", "sha256:").parse::<ContentHash>(), Ok(h), "upper-case digits");
        let good = h.to_string();
        for bad in [
            String::new(),
            good.replace("sha256:", "md5:"),
            good[..70].to_owned(),
            format!("{good}0"),
            format!("{}g", &good[..70]),
            "sha256:é".into(),
        ] {
            assert_eq!(bad.parse::<ContentHash>(), Err(HashError), "{bad:?}");
        }
        assert_eq!(serde_json::from_str::<ContentHash>(&serde_json::to_string(&h).unwrap()).unwrap(), h);
    }
}

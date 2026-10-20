//! Reading and writing image files (JPEG, PNG, TIFF, OpenEXR, WebP, BMP and DNG) with their metadata: EXIF, IPTC, XMP
//! and the embedded ICC profile, which stacked output carries over from a chosen source frame. Every format is handled in
//! pure Rust, and lossless at every bit depth the format supports. It may depend only on `florastack-core`.

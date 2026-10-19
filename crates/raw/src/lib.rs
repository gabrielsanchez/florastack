//! RAW camera files: a thin adapter over LightCraft's pure-Rust decoders, kept behind FloraStack's own decoder trait so
//! that dependency can be bumped or swapped without touching other crates, and a develop step that turns every frame of
//! a stack, with identical settings, into linear scene-referred RGB for stacking. It may depend only on
//! `florastack-core`.

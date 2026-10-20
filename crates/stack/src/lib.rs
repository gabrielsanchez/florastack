//! Stacking algorithms (weighted average, pyramid and depth map), fed one aligned linear frame at a time so memory depends
//! on image size rather than stack depth, and deterministic for a given input order and settings. Its CPU code is the
//! reference that the GPU kernels are tested against. It may depend only on `florastack-core`.

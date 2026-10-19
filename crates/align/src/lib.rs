//! Frame alignment: a per-frame shift, scale, rotation and brightness gain relative to a master frame, stack ordering and
//! output framing, and the defect handling (dust, hot pixels, subject movement) that has to run in sensor coordinates
//! before frames are warped. Its CPU code is the reference that the GPU kernels are tested against. It may depend only on
//! `florastack-core`.

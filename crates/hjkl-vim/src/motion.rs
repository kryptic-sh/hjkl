// Phase 6.6: MotionKind moved to hjkl-engine (cycle-break).
// This module re-exports from engine for any code that imports via the
// `hjkl_vim::motion::MotionKind` path. Prefer the canonical
// `hjkl_engine::MotionKind` instead.
pub use hjkl_engine::MotionKind;

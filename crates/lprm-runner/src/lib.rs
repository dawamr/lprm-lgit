//! Validation command and reviewer adapters for LPRM.

/// Returns the core version used by this adapter.
pub const fn core_version() -> &'static str {
    lprm_core::VERSION
}

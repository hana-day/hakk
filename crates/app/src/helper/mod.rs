//! Connection to the helper.
//!
//! The GUI talks to the helper only through [`hakk_protocol::Helper`]. `hakk_helper::RealHelper`
//! runs the real scan pipeline; [`mock::MockHelper`] simulates a helper with fake data so the
//! GUI can be tried without a disk.

pub mod mock;

//! Regenerates the TypeScript bindings of the helper protocol used by the web UI.
//!
//! The generated files are committed; CI runs this test and fails if they changed, so the
//! Rust types and the UI cannot drift apart.

use std::path::Path;

#[test]
fn export_typescript_bindings() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui/src/lib/bindings");
    hakk_protocol::export_typescript(&dir).expect("export TypeScript bindings");
}

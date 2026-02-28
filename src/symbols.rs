//! Symbol lookup helpers for Frida-based hooking.
//!
//! Generic utility: searches a module's exports/symbols by include/exclude patterns.
//! Reusable for any Frida hook project, not YOLO-specific.

use frida_gum::NativePointer;
use std::ffi::c_void;

/// Find a symbol in `module` whose name contains ALL `include` patterns
/// and NONE of the `exclude` patterns.
///
/// Searches exports first (faster), then falls back to full symbol table.
pub fn find_by_pattern(
    module: &frida_gum::Module,
    include: &[&str],
    exclude: &[&str],
) -> Option<(String, NativePointer)> {
    tracing::info!(
        "Searching for symbol matching {:?} (excluding {:?})",
        include, exclude
    );

    for export in module.enumerate_exports() {
        let name = &export.name;
        if include.iter().all(|pat| name.contains(pat))
            && exclude.iter().all(|pat| !name.contains(pat))
        {
            return Some((
                name.clone(),
                NativePointer(export.address as *mut c_void),
            ));
        }
    }

    tracing::info!("Not found in exports, trying symbols...");
    for sym in module.enumerate_symbols() {
        let name = &sym.name;
        if include.iter().all(|pat| name.contains(pat))
            && exclude.iter().all(|pat| !name.contains(pat))
        {
            return Some((
                name.clone(),
                NativePointer(sym.address as *mut c_void),
            ));
        }
    }

    None
}

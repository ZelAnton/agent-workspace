// ===========================================================================
// cow/detect - Same-volume probe via platform-specific device IDs
// ===========================================================================
//
// Two paths are on the same volume if their underlying storage block
// device matches. We can ask the OS:
//   - **Unix**: `stat()`'s `st_dev` field (device ID).
//   - **Windows**: `GetFileInformationByHandle`'s `dwVolumeSerialNumber`,
//     surfaced via `MetadataExt::volume_serial_number` since Rust 1.74.
//
// Both calls require the path to exist (we open it). Callers must ensure
// the inputs are real paths (typically: the repo root and the workspace
// parent dir, both of which exist by the time CoW is attempted).

use std::path::Path;

/// True if both paths resolve to the same underlying volume. Returns
/// `Some(false)` for cross-volume; `None` if metadata can't be read for
/// either path (paths missing, permission denied).
pub fn same_volume(a: &Path, b: &Path) -> Option<bool> {
    let id_a = volume_id(a)?;
    let id_b = volume_id(b)?;
    Some(id_a == id_b)
}

#[cfg(unix)]
fn volume_id(p: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    let md = std::fs::metadata(p).ok()?;
    Some(md.dev())
}

#[cfg(windows)]
fn volume_id(p: &Path) -> Option<u64> {
    // `MetadataExt::volume_serial_number` is nightly-only (#63010), so we
    // open a handle via winapi-util and pull the same field from
    // `GetFileInformationByHandle`. Returns u32 widened to u64 for
    // cross-platform unified return type.
    let handle = winapi_util::Handle::from_path_any(p).ok()?;
    let info = winapi_util::file::information(&handle).ok()?;
    Some(info.volume_serial_number())
}

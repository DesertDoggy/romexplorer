## Native library search order (app root -> shared -> system path -> dev fallback)

### Files

- `src/rom/native_lib.rs` -- add the new tiered loader.
- `src/rom/chdman_wrapper.rs`, `src/rom/dolphin_wrapper.rs`, `src/rom/sevenzip_wrapper.rs` -- switch their load call sites to use it.

### New function in `native_lib.rs`

Today, `find_native_library(release_root, base_name) -> Option<PathBuf>` only *resolves a path*; each wrapper separately calls `libloading::Library::new(path)`. The new search order needs an actual load *attempt* for the "system path" tier (there's no way to check in advance whether the OS's own dynamic loader will find a bystander-installed system library -- `Library::new(bare_filename)` *is* the check), so the tiered fallback has to own the load attempts themselves, not just path resolution. Add:

```rust
/// Tries to load `base_name`'s native library (see `dynamic_lib_filename` for the
/// per-platform filename convention), in order:
/// 1. `<exe_dir>/<filename>` -- app root, next to the running executable
///    (`std::env::current_exe()`).
/// 2. `<exe_dir>/shared/<filename>` -- a `shared/` folder alongside the executable.
/// 3. the bare filename, handed to `Library::new` with no directory component -- lets the
///    OS's own dynamic loader search its standard system library path (`PATH` search on
///    Windows, the dynamic linker's search path on Linux/macOS).
/// 4. `dev_fallback`, if given and the path exists -- each wrapper's own existing
///    submodule-relative dev build output path (`chdman_wrapper.rs`'s `release_root()` via
///    `find_native_library`, etc.). Kept as a last resort purely so `cargo test`/`cargo run`
///    keep working today without a manual copy step -- a real distributed build has no
///    `submodules/` directory, so this tier is always a no-op there.
///
/// Returns the first tier that actually loads; on total failure, an error listing every
/// tier tried and why.

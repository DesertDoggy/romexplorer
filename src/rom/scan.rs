//! Directory scanning: turning "a folder the user pointed at" into the input lists
//! a batch of [`super::operation::Single`] runs needs. Three layers (only the first two building blocks
//! are here so far -- see `docs/plan` for the full design):
//!
//! - **Layer 1** -- [`classify_directory`]: one directory, not recursing. Sorts its files
//!   into ROM candidates, BIOS/system files, playlists and unknowns; reports every
//!   subdirectory back unresolved.
//! - **Layer 2** -- the recursive walker (not yet written). Recurses to the leaves and
//!   resolves directory bundles bottom-up with [`find_bundle`].
//! - **Layer 3** -- the orchestrator (not yet written). Groups candidates and runs one
//!   batch per group.
//!
//! Two rules hold throughout: `.cue`/`.gdi` tracks are never candidates of their own
//! (`Single` already resolves a sheet's whole track set from the sheet itself), and the
//! walk root -- or anything above it -- never counts as evidence of anything.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::common::models::Platform;

use super::content::archive_format_for_extension;
use super::detect::descriptor::descriptor_payload_paths;
use super::detect::systemfile::{identify_system_file, DirType, SystemFileResult, SystemFileType};
use super::detect::{extensions_for_platform, is_known_rom_extension, HeuristicFlags};
use super::platforms::{is_directory_bundle_target, read_directory_non_disc_header_metadata};

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

/// Where a directory's platform, if any, comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlatformContext {
    /// Asserted by the caller -- the only source that ever becomes
    /// `MetadataArgs::forced_platform`, and the only one that narrows which extensions a
    /// directory is scanned for.
    Forced(Platform),
    /// Resolved from a platform folder strictly inside the scan -- tried first, never
    /// trusted (`MetadataArgs::preferred_platform`). Doesn't narrow extensions: a
    /// mislabeled folder must not make real files disappear.
    Hint(Platform),
    None,
}

impl PlatformContext {
    pub fn platform(self) -> Option<Platform> {
        match self {
            Self::Forced(platform) | Self::Hint(platform) => Some(platform),
            Self::None => None,
        }
    }
}

/// One declared content shape (dimension 4 of a scan): what games in this collection look
/// like on disk, as the caller knows it. Written like `msx, rom, dir(rom), dir(dir(raw, rom))`
/// -- see [`RomShape::parse_list`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RomShape {
    /// A standalone file with this lowercase extension (`""`: no extension, written
    /// `noext`).
    File(String),
    /// A directory that is one whole game. Strict: every entry in it (dotfiles aside) must
    /// match one of these inner shapes -- `dir(dir(raw))`'s innermost directory holds only
    /// `.raw` files, no subdirectory and no other file. A `.rom` that may also appear has to
    /// be declared (`dir(dir(raw, rom))`).
    Dir(Vec<RomShape>),
}

impl RomShape {
    /// Parses a comma-separated shape list, e.g. `msx, rom, bin, dir(rom), dir(dir(raw, rom))`.
    /// Case-insensitive; whitespace ignored; `noext` is a file with no extension; `dir` not
    /// followed by `(` is just the extension `dir`.
    pub fn parse_list(text: &str) -> Result<Vec<RomShape>, String> {
        let chars: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).map(|c| c.to_ascii_lowercase()).collect();
        let mut pos = 0;
        let shapes = parse_shape_list(&chars, &mut pos)?;
        if pos != chars.len() {
            return Err(format!("unexpected '{}' at position {pos}", chars[pos]));
        }
        Ok(shapes)
    }

    /// How many directory levels this shape spans: 0 for a file, 1 for `dir(rom)`, 2 for
    /// `dir(dir(raw))`.
    pub fn depth(&self) -> usize {
        match self {
            Self::File(_) => 0,
            Self::Dir(inner) => 1 + inner.iter().map(Self::depth).max().unwrap_or(0),
        }
    }

    /// Whether `path` has exactly this shape (see [`RomShape::Dir`] for the strictness rule).
    pub fn matches(&self, path: &Path) -> bool {
        match self {
            Self::File(ext) => path.is_file() && extension_lowercase(path) == *ext,
            Self::Dir(inner) => {
                if !path.is_dir() {
                    return false;
                }
                let entries = visible_entries(path);
                !entries.is_empty() && entries.iter().all(|(entry, _)| inner.iter().any(|shape| shape.matches(entry)))
            }
        }
    }
}

fn parse_shape_list(chars: &[char], pos: &mut usize) -> Result<Vec<RomShape>, String> {
    let mut shapes = vec![parse_shape(chars, pos)?];
    while chars.get(*pos) == Some(&',') {
        *pos += 1;
        shapes.push(parse_shape(chars, pos)?);
    }
    Ok(shapes)
}

fn parse_shape(chars: &[char], pos: &mut usize) -> Result<RomShape, String> {
    let start = *pos;
    while chars.get(*pos).is_some_and(|c| c.is_ascii_alphanumeric()) {
        *pos += 1;
    }
    let token: String = chars[start..*pos].iter().collect();
    if token == "dir" && chars.get(*pos) == Some(&'(') {
        *pos += 1;
        let inner = parse_shape_list(chars, pos)?;
        if chars.get(*pos) != Some(&')') {
            return Err(format!("missing ')' at position {pos}"));
        }
        *pos += 1;
        return Ok(RomShape::Dir(inner));
    }
    match token.as_str() {
        "" => Err(format!("expected a shape at position {start}")),
        "noext" => Ok(RomShape::File(String::new())),
        _ => Ok(RomShape::File(token)),
    }
}

/// Everything one directory level is classified with. Flows top-down: the orchestrator
/// builds it from the scan request, the walker narrows `platform` per subtree.
#[derive(Debug, Clone)]
pub struct ScanContext {
    /// Whether this directory holds BIOS/system files, ROMs, or both. Only `Bios`/`Rom`
    /// run the BIOS heuristic (`detect::systemfile`); in `Mixed`, BIOS files are left to be
    /// identified later by hash, like any unknown file.
    pub dir_type: DirType,
    pub platform: PlatformContext,
    /// The caller's extension/path heuristic switches, passed through unchanged.
    pub heuristics: HeuristicFlags,
    /// Dimension 4: the content shapes the caller declared for this collection.
    pub shapes: Vec<RomShape>,
    /// Counts the files the walk has listed so far, for a caller that wants to show the
    /// walk moving: the walk itself only returns when it is finished, and on a large
    /// collection (or a network share) that is long enough to look stuck. `None`: nobody
    /// is watching.
    pub files_seen: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    /// Directories below the scan root configured with settings of their own (a type, a
    /// platform): their subtree is walked with those, as a scan of that directory would, and
    /// each is a boundary no bundle reaches above. Empty: nothing configured below.
    pub configured: std::sync::Arc<Vec<ConfiguredDir>>,
}

/// A configured directory below a scan root, with its effective settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredDir {
    pub path: PathBuf,
    pub dir_type: DirType,
    pub platform: Option<Platform>,
}

impl ScanContext {
    /// The context and bundle boundary for `subdir`: its own configured settings when it has
    /// any, else this context and `boundary` unchanged.
    fn entering(&self, subdir: &Path, boundary: &Path) -> (ScanContext, PathBuf) {
        match self.configured.iter().find(|c| c.path == subdir) {
            Some(c) => {
                let platform = c.platform.map(PlatformContext::Forced).unwrap_or(self.platform);
                (ScanContext { dir_type: c.dir_type, platform, ..self.clone() }, subdir.to_path_buf())
            }
            None => (self.clone(), boundary.to_path_buf()),
        }
    }
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CandidateKind {
    File,
    /// A `.cue`/`.gdi`: its tracks come with it (`InputSet::resolve`), never listed apart.
    Sheet,
    /// A whole directory as one game (`InputKind::Directory` downstream).
    DirectoryBundle,
    /// A file the directory's context didn't accept (unknown extension, or a `Mixed`/
    /// unidentified file), still processed because `skip_unknown` was false: hashed and
    /// detected normally, but never converted or archived to a platform-driven container
    /// (CHD/RVZ/GCZ/WIA) -- see [`unknown_archive_args`].
    Unknown,
}

/// One input a `Single` will run on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RomCandidate {
    pub path: PathBuf,
    pub kind: CandidateKind,
    /// A platform found for this candidate specifically (a bundle confirmed by its
    /// platform's own signature), beyond whatever its directory's context says.
    pub platform_hint: Option<Platform>,
}

impl RomCandidate {
    pub fn of(path: PathBuf) -> Self {
        let kind = if path.is_dir() {
            CandidateKind::DirectoryBundle
        } else if is_sheet_extension(&extension_lowercase(&path)) {
            CandidateKind::Sheet
        } else {
            CandidateKind::File
        };
        Self { path, kind, platform_hint: None }
    }
}

/// An `.m3u` and the discs it lists: each disc is its own candidate (a disc is a complete
/// input on its own, unlike a cue's tracks); this records that they're one game.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaylistGroup {
    pub m3u_path: PathBuf,
    /// Listed entries that exist, in playlist order.
    pub member_paths: Vec<PathBuf>,
    /// Listed entries that don't exist (e.g. a multi-disc set missing a disc).
    pub missing_members: Vec<PathBuf>,
}

/// [`classify_directory`]'s result for one directory.
#[derive(Debug, Clone, Default)]
pub struct DirectoryClassification {
    pub rom_candidates: Vec<RomCandidate>,
    pub playlists: Vec<PlaylistGroup>,
    /// Files the BIOS heuristic identified (`Bios`/`Rom` directories only).
    pub system_files: Vec<(PathBuf, SystemFileResult)>,
    /// Every subdirectory -- the walker decides whether to recurse, and resolves bundles.
    pub unresolved_subdirs: Vec<PathBuf>,
    /// Paths another candidate already covers (a cue/gdi's tracks). Never a candidate
    /// anywhere in a scan, even when reached again from another directory.
    pub consumed: Vec<PathBuf>,
    /// Files this directory's context doesn't take (non-ROM extensions, unidentified files
    /// in a BIOS directory, unreadable playlists) -- left for hash-based identification.
    pub unknown: Vec<PathBuf>,
}

// ---------------------------------------------------------------------------
// Layer 1
// ---------------------------------------------------------------------------

/// Classifies the entries of one directory, without recursing (see the module doc).
pub fn classify_directory(dir: &Path, ctx: &ScanContext) -> DirectoryClassification {
    classify_listing(visible_entries(dir), ctx)
}

/// [`classify_directory`] on a listing already read ([`visible_entries`]): the walker reads
/// each directory once, for both its subdirectories and its files.
fn classify_listing(listing: Vec<(PathBuf, bool)>, ctx: &ScanContext) -> DirectoryClassification {
    let mut out = DirectoryClassification::default();
    let (subdirs, files): (Vec<PathBuf>, Vec<PathBuf>) = {
        let (dirs, files): (Vec<_>, Vec<_>) = listing.into_iter().partition(|(_, is_dir)| *is_dir);
        (dirs.into_iter().map(|(p, _)| p).collect(), files.into_iter().map(|(p, _)| p).collect())
    };
    if let Some(seen) = &ctx.files_seen {
        seen.fetch_add(files.len() as u64, std::sync::atomic::Ordering::Relaxed);
    }

    // Tracks first, so a sheet's own tracks never become candidates below.
    let mut consumed: HashSet<PathBuf> = HashSet::new();
    // A sheet naming `GAME.BIN` for `Game.bin` finds it on a case-insensitive disk (Windows,
    // macOS); its track is the entry as listed, or it would be read and recorded twice.
    let as_listed: HashMap<String, &PathBuf> = files.iter().map(|f| (f.to_string_lossy().to_lowercase(), f)).collect();
    for sheet in files.iter().filter(|file| is_sheet_extension(&extension_lowercase(file))) {
        consumed.extend(descriptor_payload_paths(sheet).into_iter().map(|track| as_listed.get(&track.to_string_lossy().to_lowercase()).map(|f| (*f).clone()).unwrap_or(track)));
    }

    let mut listed: HashSet<PathBuf> = HashSet::new();
    for playlist in files.iter().filter(|file| extension_lowercase(file) == "m3u") {
        let Ok(entries) = parse_m3u(playlist) else {
            out.unknown.push(playlist.clone());
            continue;
        };
        let mut group = PlaylistGroup { m3u_path: playlist.clone(), ..Default::default() };
        for member in entries {
            // Spelled as listed, where the disk says the two are one file (case-insensitive).
            let member = match as_listed.get(&member.to_string_lossy().to_lowercase()) {
                Some(listed) if member.exists() => (*listed).clone(),
                _ => member,
            };
            if consumed.contains(&member) {
                continue;
            }
            if !member.exists() {
                group.missing_members.push(member);
                continue;
            }
            if listed.insert(member.clone()) {
                out.rom_candidates.push(RomCandidate { platform_hint: ctx.platform.platform(), ..RomCandidate::of(member.clone()) });
            }
            group.member_paths.push(member);
        }
        out.playlists.push(group);
    }

    let platform = ctx.platform.platform();
    for file in files {
        let ext = extension_lowercase(&file);
        if ext == "m3u" || consumed.contains(&file) || listed.contains(&file) {
            continue;
        }
        match ctx.dir_type {
            DirType::Bios => {
                let result = identify_system_file(&file, DirType::Bios, platform);
                if result.file_type == SystemFileType::Unknown {
                    out.unknown.push(file);
                } else {
                    out.system_files.push((file, result));
                }
                continue;
            }
            DirType::Rom => {
                // Before the extension filter: BIOS dumps use extensions no ROM table lists
                // (PS2's .EROM/.NVM/...), and only small files are hashed at all.
                let result = identify_system_file(&file, DirType::Rom, platform);
                if result.file_type != SystemFileType::Unknown {
                    out.system_files.push((file, result));
                    continue;
                }
            }
            DirType::Mixed => {}
        }
        if accepts_extension(&ext, ctx) {
            // The directory's platform (forced or hinted) travels with the file as a hint;
            // whoever records it still checks the file's own signature before believing it.
            out.rom_candidates.push(RomCandidate { platform_hint: platform, ..RomCandidate::of(file) });
        } else {
            out.unknown.push(file);
        }
    }

    out.unresolved_subdirs = subdirs;
    let mut consumed: Vec<PathBuf> = consumed.into_iter().collect();
    consumed.sort();
    out.consumed = consumed;
    out
}

/// Whether a file with this extension is a ROM candidate in `ctx`. Containers are always
/// taken (their members decide); a forced platform narrows to that platform's own
/// extensions; a hint or no platform takes any extension any platform uses. Declared
/// `File` shapes always count; with the extension heuristic off, everything does.
fn accepts_extension(ext: &str, ctx: &ScanContext) -> bool {
    if !ctx.heuristics.ext {
        return true;
    }
    if ctx.shapes.iter().any(|shape| matches!(shape, RomShape::File(declared) if declared == ext)) {
        return true;
    }
    if ext.is_empty() {
        return false;
    }
    if archive_format_for_extension(ext).is_some() {
        return true;
    }
    match ctx.platform {
        PlatformContext::Forced(platform) => extensions_for_platform(platform).contains(&ext),
        PlatformContext::Hint(_) | PlatformContext::None => is_known_rom_extension(ext),
    }
}

// ---------------------------------------------------------------------------
// Bundles (for the walker)
// ---------------------------------------------------------------------------

/// A directory confirmed as one whole game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    pub path: PathBuf,
    /// Set when the platform's own bundle signature confirmed it.
    pub platform_hint: Option<Platform>,
}

/// Whether `dir` -- a leaf (no subdirectories, only files) strictly below `boundary` -- is
/// itself one whole game: single-level only, either by a declared depth-1 `Dir` shape, or
/// by the context's forced/hinted platform's own directory-bundle signature.
///
/// Deliberately does not climb to check an enclosing directory (`game/{disk1/, disk2/}`
/// collapsing into one `game` candidate) -- confirmed by the user: CD-based multi-disc
/// games are already fully handled by `.cue`/`.gdi` + `.m3u` (no directory bundling
/// needed), and linking several depth-1 bundles (e.g. mycom disk folders) into one game is
/// a database/import-level concern (`database::import::release_name_of`'s existing
/// disc-tag rollup), not this scan's.
///
/// `dir` must be a genuine leaf when this is called -- the signature check
/// (`read_directory_non_disc_header_metadata`/`download::verdict_for_directory`) scans
/// *recursively* for a match anywhere inside the given directory, so calling this on a
/// directory that still has subdirectories of its own (e.g. a whole platform folder of
/// several separate games) could find one game's signature deep inside and wrongly
/// swallow the entire folder as one bundle. The walker only ever calls this on a directory
/// it has already confirmed has no subdirectories.
///
/// Never at or above `boundary` (the walk root, or `root/<platform>` under platform-
/// separated scans): a platform folder of loose games can never itself become one bundle,
/// even if it happens to be a leaf (no subdirectories at all -- every game sits flat).
/// `dir` and `boundary` must be in the same form (both absolute, or both relative to the
/// same base) -- this compares paths, it doesn't resolve them.
pub fn bundle_at(dir: &Path, boundary: &Path, ctx: &ScanContext) -> Option<Bundle> {
    if !is_strictly_below(dir, boundary) {
        return None;
    }
    if let Some(platform) = signature_bundle(dir, ctx) {
        return Some(Bundle { path: dir.to_path_buf(), platform_hint: Some(platform) });
    }
    let by_shape = ctx.shapes.iter().any(|shape| shape.depth() == 1 && shape.matches(dir));
    by_shape.then(|| Bundle { path: dir.to_path_buf(), platform_hint: None })
}

/// The folder's platform when its own signature -- or that of the same console's other
/// platform (DSi / DSiWare, `download::same_console`) -- says the folder is one game. The
/// folder's platform is kept: that is where the collection files it, and its DAT.
fn signature_bundle(dir: &Path, ctx: &ScanContext) -> Option<Platform> {
    let platform = ctx.platform.platform()?;
    if !is_directory_bundle_target(platform) {
        return None;
    }
    let sibling = crate::rom::platforms::download::same_console(platform);
    std::iter::once(platform).chain(sibling).any(|p| read_directory_non_disc_header_metadata(dir, p).is_some()).then_some(platform)
}

/// A folder whose own root is a game's: `sce_sys/param.sfo` with Vita keys (an app, patch or
/// add-on folder as a Vita or NoNpDrm dump lays it out), `PS3_GAME/PARAM.SFO` (a PS3 disc or
/// hard-disk game copied out as files) or `PSP_GAME/PARAM.SFO`. Unlike [`bundle_at`] this
/// takes folders with subfolders -- these trees always have them -- because the signature has
/// to sit at the folder's own top, not anywhere inside it. A forced platform that is another
/// one wins.
pub fn game_root_bundle(dir: &Path, boundary: &Path, ctx: &ScanContext) -> Option<Bundle> {
    use crate::rom::platforms::sony::Sfo;
    if !is_strictly_below(dir, boundary) {
        return None;
    }
    let sfo_at = |relative: &str| -> Option<Sfo> {
        let path = dir.join(relative);
        let meta = fs::metadata(&path).ok()?;
        (meta.is_file() && meta.len() <= 0x10000).then(|| fs::read(&path).ok().and_then(|bytes| Sfo::parse(&bytes))).flatten()
    };
    let platform = if let Some(sfo) = sfo_at("sce_sys/param.sfo") {
        (sfo.text("TITLE_ID").is_some() && (sfo.is_vita() || matches!(sfo.text("CATEGORY"), Some("gd" | "gc" | "gp" | "ac")))).then_some(Platform::PlayStationVita)?
    } else if let Some(sfo) = sfo_at("PS3_GAME/PARAM.SFO") {
        sfo.text("TITLE_ID").is_some().then_some(Platform::PlayStation3)?
    } else if let Some(sfo) = sfo_at("PSP_GAME/PARAM.SFO") {
        (sfo.text("DISC_ID").is_some() || sfo.text("TITLE_ID").is_some()).then_some(Platform::PlayStationPortable)?
    } else {
        return None;
    };
    if matches!(ctx.platform, PlatformContext::Forced(forced) if forced != platform) {
        return None;
    }
    Some(Bundle { path: dir.to_path_buf(), platform_hint: Some(platform) })
}

fn is_strictly_below(dir: &Path, boundary: &Path) -> bool {
    dir != boundary && dir.starts_with(boundary)
}

// ---------------------------------------------------------------------------
// Layer 2: the walker
// ---------------------------------------------------------------------------

/// How platforms are laid out under a scan root -- resolved once per subtree into the
/// [`PlatformContext`] each directory below it is classified/bundled with.
#[derive(Debug, Clone)]
pub enum PlatformLayout {
    /// The whole scan is one already-known platform -- "platform force, so less detect
    /// overhead." Never re-resolved.
    Forced(Platform),
    /// Immediate subdirectories of the root are separate platforms, resolved per
    /// subdirectory (an explicit name -> platform map; a subdirectory missing from it
    /// falls back to [`crate::rom::detect::infer_platform_from_parent_path_heuristics`]
    /// on that subdirectory's own name when `path_heuristic` is on, else stays `None`).
    /// The root itself is never path-heuristic evidence -- only directories strictly
    /// inside the scan are.
    SeparatedBySubdir(std::collections::HashMap<String, Platform>),
    /// Not separated -- every candidate is detected independently, with no hint at all.
    Unseparated,
}

/// Everything one whole scan collected.
#[derive(Debug, Clone, Default)]
pub struct WalkResult {
    pub rom_candidates: Vec<RomCandidate>,
    pub playlists: Vec<PlaylistGroup>,
    pub system_files: Vec<(PathBuf, SystemFileResult)>,
    /// Only populated when `skip_unknown` is true (a pure report, not fed to a batch);
    /// otherwise these paths are in `rom_candidates` as `CandidateKind::Unknown` instead.
    pub unknown: Vec<PathBuf>,
    /// Every path in `rom_candidates` / `unknown`, so adding one is a lookup rather than a scan
    /// of everything found so far (quadratic over a large collection).
    seen_candidates: HashSet<PathBuf>,
    seen_unknown: HashSet<PathBuf>,
    /// Every sheet track found so far, from any directory: a track a sheet names in another
    /// folder (`../x.bin`, `Tracks/x.bin`) is the sheet's, not a candidate of its own,
    /// whichever of the two folders the walk reaches first.
    consumed: HashSet<PathBuf>,
    /// The directories walked, by their resolved path: a linked folder pointing at a sibling
    /// (or anywhere else already walked) lists the same files again under another path.
    walked: HashSet<PathBuf>,
}

/// Runs a whole scan from `root`, resolving `layout` into a per-subtree
/// [`PlatformContext`] and recursing. `skip_unknown` (default should be `false` at the
/// caller): when `false`, a directory's unaccepted files still become `CandidateKind::
/// Unknown` candidates (hashed and detected, never converted/archived to a platform-driven
/// container -- see [`unknown_archive_args`]) instead of only being reported.
pub fn scan(root: &Path, layout: &PlatformLayout, base_ctx: &ScanContext, skip_unknown: bool) -> WalkResult {
    let mut out = WalkResult::default();
    match layout {
        PlatformLayout::Forced(platform) => {
            let ctx = ScanContext { platform: PlatformContext::Forced(*platform), ..base_ctx.clone() };
            walk(root, root, &ctx, skip_unknown, &mut out);
        }
        PlatformLayout::Unseparated => {
            let ctx = ScanContext { platform: PlatformContext::None, ..base_ctx.clone() };
            walk(root, root, &ctx, skip_unknown, &mut out);
        }
        PlatformLayout::SeparatedBySubdir(known) => {
            // Files directly in the root (no platform of its own) -- rare in a genuinely
            // platform-separated layout, but not an error; classified, never bundled (the
            // root is always the boundary for every subtree below it).
            let root_ctx = ScanContext { platform: PlatformContext::None, ..base_ctx.clone() };
            out.walked.insert(resolved(root));
            let root_classified = classify_directory(root, &root_ctx);
            let root_subdirs = root_classified.unresolved_subdirs.clone();
            merge_classification(&mut out, root_classified, skip_unknown);

            for subdir in root_subdirs {
                if links_back(&subdir) {
                    continue;
                }
                let name = subdir.file_name().and_then(|n| n.to_str()).unwrap_or("");
                // infer_platform_from_parent_path_heuristics reads its argument's *parent*
                // directory name (it's meant for a file path) -- `.join("_")` makes that
                // parent `subdir` itself, so this reads `subdir`'s own name.
                let platform = known.get(name).copied().or_else(|| {
                    base_ctx.heuristics.path.then(|| super::detect::infer_platform_from_parent_path_heuristics(&subdir.join("_"))).flatten()
                });
                let ctx = ScanContext { platform: platform.map(PlatformContext::Hint).unwrap_or(PlatformContext::None), ..base_ctx.clone() };
                let (ctx, _) = ctx.entering(&subdir, &subdir);
                let platform = platform.or(ctx.platform.platform());
                // A folder no platform is known for may be a game itself (a PS3, PSP or Vita
                // game folder, a declared shape) sitting straight in the root: tried as one
                // before it is walked as a platform folder, or its files became games each.
                if platform.is_none() {
                    let bundle = game_root_bundle(&subdir, root, &ctx).or_else(|| {
                        let leaf = visible_entries(&subdir).iter().all(|(_, is_dir)| !is_dir);
                        leaf.then(|| bundle_at(&subdir, root, &ctx)).flatten()
                    });
                    if let Some(bundle) = bundle {
                        if out.walked.insert(resolved(&subdir)) {
                            push_candidate(&mut out, RomCandidate { path: bundle.path, kind: CandidateKind::DirectoryBundle, platform_hint: bundle.platform_hint });
                        }
                        continue;
                    }
                }
                walk(&subdir, &subdir, &ctx, skip_unknown, &mut out);
            }
        }
    }
    out
}

/// Depth-first: classifies `dir` (Layer 1), then for each subdirectory either resolves it
/// as one bundle (a leaf that passes [`bundle_at`]) or recurses into it. Paths already seen
/// anywhere in this walk (an `.m3u`/symlink reaching the same file twice) are skipped.
pub fn walk(dir: &Path, boundary: &Path, ctx: &ScanContext, skip_unknown: bool, out: &mut WalkResult) {
    let at = resolved(dir);
    if !out.walked.insert(at.clone()) {
        return;
    }
    walk_listed(visible_entries(dir), &at, boundary, ctx, skip_unknown, out);
}

/// `dir` with its links resolved, or as given when that fails.
fn resolved(dir: &Path) -> PathBuf {
    fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

/// [`walk`] on a directory whose listing is already read: each directory is listed once --
/// over a network share every listing is a round trip. `at` is the directory's resolved
/// path: a plain subfolder's is `at` plus its name, so only a link costs a lookup.
fn walk_listed(listing: Vec<(PathBuf, bool)>, at: &Path, boundary: &Path, ctx: &ScanContext, skip_unknown: bool, out: &mut WalkResult) {
    let classified = classify_listing(listing, ctx);
    // Real folders before links, so a link to a sibling is the copy that is skipped.
    let mut subdirs: Vec<(PathBuf, bool)> = classified.unresolved_subdirs.iter().map(|d| (d.clone(), fs::symlink_metadata(d).is_ok_and(|m| m.file_type().is_symlink()))).collect();
    subdirs.sort_by_key(|(_, link)| *link);
    merge_classification(out, classified, skip_unknown);

    for (subdir, link) in subdirs {
        if link && links_back(&subdir) {
            continue;
        }
        let sub_at = if link { resolved(&subdir) } else { subdir.file_name().map(|n| at.join(n)).unwrap_or_else(|| resolved(&subdir)) };
        if !out.walked.insert(sub_at.clone()) {
            continue;
        }
        let (ctx, boundary) = ctx.entering(&subdir, boundary);
        let (ctx, boundary) = (&ctx, boundary.as_path());
        // A game's own tree (a Vita app folder, a PS3 or PSP game folder) is one game however
        // deep it goes: its root says so, and nothing below it is a game of its own.
        if let Some(bundle) = game_root_bundle(&subdir, boundary, ctx) {
            push_candidate(out, RomCandidate { path: bundle.path, kind: CandidateKind::DirectoryBundle, platform_hint: bundle.platform_hint });
            continue;
        }
        let listing = visible_entries(&subdir);
        // A leaf (no subdirectories) may be one game as a whole; a leaf that isn't is just a
        // directory of loose files, classified like any other.
        if listing.iter().all(|(_, is_dir)| !is_dir) {
            if let Some(bundle) = bundle_at(&subdir, boundary, ctx) {
                push_candidate(out, RomCandidate { path: bundle.path, kind: CandidateKind::DirectoryBundle, platform_hint: bundle.platform_hint });
                continue;
            }
        }
        walk_listed(listing, &sub_at, boundary, ctx, skip_unknown, out);
    }
}

/// A linked directory that points at one of the folders it sits in (`roms/all -> ..`): walking
/// it would list the same files again under a longer path, about forty times over until the
/// system's link limit. Only links cost a lookup.
fn links_back(dir: &Path) -> bool {
    if !fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink()) {
        return false;
    }
    match (fs::canonicalize(dir), dir.parent().map(fs::canonicalize)) {
        (Ok(target), Some(Ok(parent))) => parent.starts_with(&target),
        _ => true,
    }
}

fn merge_classification(out: &mut WalkResult, classified: DirectoryClassification, skip_unknown: bool) {
    // A track named from another folder that the walk already took as a file of its own.
    let late: Vec<&PathBuf> = classified.consumed.iter().filter(|t| out.seen_candidates.contains(*t) || out.seen_unknown.contains(*t)).collect();
    if !late.is_empty() {
        let late: HashSet<&PathBuf> = late.into_iter().collect();
        out.rom_candidates.retain(|c| !late.contains(&c.path));
        out.unknown.retain(|p| !late.contains(p));
    }
    out.consumed.extend(classified.consumed);
    out.playlists.extend(classified.playlists);
    out.system_files.extend(classified.system_files);
    for candidate in classified.rom_candidates {
        push_candidate(out, candidate);
    }
    if skip_unknown {
        for path in classified.unknown {
            if !out.consumed.contains(&path) && out.seen_unknown.insert(path.clone()) {
                out.unknown.push(path);
            }
        }
    } else {
        for path in classified.unknown {
            push_candidate(out, RomCandidate { path, kind: CandidateKind::Unknown, platform_hint: None });
        }
    }
}

/// Adds `candidate` unless its path is already present anywhere in this walk's results, in
/// the order found.
fn push_candidate(out: &mut WalkResult, candidate: RomCandidate) {
    if out.consumed.contains(&candidate.path) {
        return;
    }
    if out.seen_candidates.insert(candidate.path.clone()) {
        out.rom_candidates.push(candidate);
    }
}

/// The `ArchiveArgs` an unidentified (`CandidateKind::Unknown`) candidate should actually
/// run with: zip/7z pass through from `base` unchanged (they need no platform), but the
/// platform-driven targets are forced off regardless of `base` -- CHD/RVZ/GCZ/WIA's
/// internal layout depends on knowing which platform the content is, which is exactly what
/// `Unknown` means there isn't one of.
pub fn unknown_archive_args(base: &super::models::ArchiveArgs) -> super::models::ArchiveArgs {
    super::models::ArchiveArgs { do_chd: false, do_rvz: false, do_gcz: false, do_wia: false, ..base.clone() }
}

// ---------------------------------------------------------------------------
// Playlists
// ---------------------------------------------------------------------------

/// The entries of an `.m3u` playlist, resolved against its own directory: one path per
/// line; blank lines, `#` comment/directive lines (`#EXTM3U`, `#EXTINF`) and URLs skipped;
/// a UTF-8 BOM tolerated; `\` separators (playlists written on Windows) read as `/`
/// elsewhere, and each name matched to the disk's own spelling ignoring case
/// ([`resolve_listed`](super::detect::sheet::resolve_listed)). Entries that don't exist are
/// still returned -- the caller decides what a missing disc means.
pub fn parse_m3u(path: &Path) -> Result<Vec<PathBuf>, String> {
    let bytes = fs::read(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(&text);
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let mut listings = super::detect::sheet::Listings::default();
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.contains("://"))
        .map(|line| normalized(&super::detect::sheet::resolve_listed(base, line, &mut listings)))
        .collect())
}

/// `a/./b/../c` as `a/c`, without touching the disk: an m3u entry written `./Disc 1.chd` is
/// the same path as the disc the walk lists.
fn normalized(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir if matches!(out.components().next_back(), Some(Component::Normal(_))) => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn extension_lowercase(path: &Path) -> String {
    path.extension().and_then(|ext| ext.to_str()).unwrap_or("").to_ascii_lowercase()
}

fn is_sheet_extension(ext: &str) -> bool {
    matches!(ext, "cue" | "gdi")
}

/// A directory's entries minus dotfiles, sorted, each with whether it is a directory -- the
/// same visibility rule `platforms::directory_payload_paths` uses. An unreadable directory
/// has no entries. The kind comes with the listing itself; only a symlink costs a lookup of
/// its own (over a share, a lookup per entry is what made a walk slower than the file
/// manager's count of the same folder).
fn visible_entries(dir: &Path) -> Vec<(PathBuf, bool)> {
    let Ok(read) = fs::read_dir(dir) else { return Vec::new() };
    let mut entries: Vec<(PathBuf, bool)> = read
        .flatten()
        .filter(|entry| !entry.file_name().to_str().is_some_and(|name| name.starts_with('.')))
        .map(|entry| {
            let path = entry.path();
            let is_dir = match entry.file_type() {
                Ok(t) if t.is_symlink() => path.is_dir(),
                Ok(t) => t.is_dir(),
                Err(_) => path.is_dir(),
            };
            (path, is_dir)
        })
        .collect();
    entries.sort();
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rom::platforms::sony::Sfo;

    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("romexplorer_test_scan_{}_{name}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create scratch dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        /// Creates `relative` (and its parent directories) holding `contents`.
        fn file(&self, relative: &str, contents: &[u8]) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).expect("create parent");
            fs::write(&path, contents).expect("write scratch file");
            path
        }

        fn dir(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(&path).expect("create scratch subdir");
            path
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn ctx(dir_type: DirType, platform: PlatformContext, shapes: &str) -> ScanContext {
        let shapes = if shapes.is_empty() { Vec::new() } else { RomShape::parse_list(shapes).expect("valid shapes") };
        ScanContext { dir_type, platform, heuristics: HeuristicFlags::ALL, shapes, files_seen: None, configured: Default::default() }
    }

    fn candidate_paths_walk(out: &WalkResult) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = out.rom_candidates.iter().map(|c| c.path.clone()).collect();
        paths.sort();
        paths
    }

    fn candidate_paths(out: &DirectoryClassification) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = out.rom_candidates.iter().map(|c| c.path.clone()).collect();
        paths.sort();
        paths
    }

    // -- shapes --------------------------------------------------------------

    #[test]
    fn parses_the_shape_notation() {
        use RomShape::{Dir, File};
        let parsed = RomShape::parse_list("msx, ROM, bin, dir(rom), dir(dir(raw, rom)), noext, dir").unwrap();
        assert_eq!(
            parsed,
            vec![
                File("msx".into()),
                File("rom".into()),
                File("bin".into()),
                Dir(vec![File("rom".into())]),
                Dir(vec![Dir(vec![File("raw".into()), File("rom".into())])]),
                File(String::new()),
                File("dir".into()),
            ]
        );
        assert_eq!(parsed[3].depth(), 1);
        assert_eq!(parsed[4].depth(), 2);
        for bad in ["dir(rom", "rom,", ",rom", "dir()", "rom)"] {
            assert!(RomShape::parse_list(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn dir_shapes_are_strict() {
        let scratch = ScratchDir::new("shape_strict");
        let shape = &RomShape::parse_list("dir(dir(raw))").unwrap()[0];
        scratch.file("game/disk1/a.raw", b"x");
        scratch.file("game/disk2/b.raw", b"x");
        scratch.file("game/disk2/.hidden", b"x"); // dotfiles don't count
        let game = scratch.path().join("game");
        assert!(shape.matches(&game));

        scratch.file("game/disk2/c.rom", b"x"); // undeclared extension in the innermost dir
        assert!(!shape.matches(&game));
        assert!(RomShape::parse_list("dir(dir(raw, rom))").unwrap()[0].matches(&game), "declared, it matches");

        scratch.file("game/notes.txt", b"x"); // a file where only directories may be
        assert!(!RomShape::parse_list("dir(dir(raw, rom))").unwrap()[0].matches(&game));

        let empty = scratch.dir("empty");
        assert!(!RomShape::parse_list("dir(raw)").unwrap()[0].matches(&empty), "an empty directory is no game");
    }

    // -- Layer 1 -------------------------------------------------------------

    #[test]
    fn plain_files_are_taken_by_extension() {
        let scratch = ScratchDir::new("plain");
        let gba = scratch.file("a.gba", b"x");
        let md = scratch.file("b.md", b"x");
        let zip = scratch.file("c.zip", b"x");
        let txt = scratch.file("readme.txt", b"x");
        scratch.file(".hidden.gba", b"x");
        let sub = scratch.dir("sub");

        let out = classify_directory(scratch.path(), &ctx(DirType::Mixed, PlatformContext::None, ""));
        assert_eq!(candidate_paths(&out), vec![gba, md, zip]);
        assert_eq!(out.unknown, vec![txt]);
        assert_eq!(out.unresolved_subdirs, vec![sub], "subdirectories are always the walker's");
    }

    #[test]
    fn a_sheets_tracks_are_consumed_never_candidates() {
        let scratch = ScratchDir::new("sheet");
        let cue = scratch.file("game.cue", b"FILE \"game (Track 1).bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n");
        let track = scratch.file("game (Track 1).bin", b"x");
        let stray = scratch.file("other.bin", b"x");

        let out = classify_directory(scratch.path(), &ctx(DirType::Mixed, PlatformContext::None, ""));
        assert_eq!(candidate_paths(&out), vec![cue.clone(), stray]);
        assert_eq!(out.rom_candidates.iter().find(|c| c.path == cue).unwrap().kind, CandidateKind::Sheet);
        assert_eq!(out.consumed, vec![track]);
    }

    #[test]
    fn a_playlists_discs_are_candidates_and_grouped() {
        let scratch = ScratchDir::new("playlist");
        let disc1 = scratch.file("game (Disc 1).chd", b"x");
        let disc2 = scratch.file("game (Disc 2).chd", b"x");
        let m3u = scratch.file("game.m3u", b"\xEF\xBB\xBF#EXTM3U\ngame (Disc 1).chd\n\n# comment\ngame (Disc 2).chd\ngame (Disc 3).chd\n");

        let out = classify_directory(scratch.path(), &ctx(DirType::Mixed, PlatformContext::None, ""));
        assert_eq!(candidate_paths(&out), vec![disc1.clone(), disc2.clone()], "each disc once; the m3u itself isn't one");
        assert_eq!(out.playlists.len(), 1);
        let group = &out.playlists[0];
        assert_eq!(group.m3u_path, m3u);
        assert_eq!(group.member_paths, vec![disc1, disc2]);
        assert_eq!(group.missing_members, vec![scratch.path().join("game (Disc 3).chd")]);
    }

    #[test]
    fn m3u_entries_resolve_relative_and_windows_separators() {
        let scratch = ScratchDir::new("m3u_paths");
        let m3u = scratch.file("list.m3u", b"discs\\a.chd\r\nhttp://example.invalid/x.chd\r\n");
        let entries = parse_m3u(&m3u).unwrap();
        let expected = if cfg!(windows) { scratch.path().join("discs\\a.chd") } else { scratch.path().join("discs/a.chd") };
        assert_eq!(entries, vec![expected], "URLs skipped, CRLF handled");
    }

    /// A playlist naming its discs in another case than the disk finds them on Linux too, and
    /// names them as the walk lists them, so a disc is not a member and a candidate apart.
    #[test]
    fn m3u_entries_match_the_disk_ignoring_case() {
        let scratch = ScratchDir::new("m3u_case");
        let disc = scratch.file("Game (Disc 1).chd", b"x");
        scratch.file("game.m3u", b"GAME (DISC 1).CHD\n");
        let out = classify_directory(scratch.path(), &ctx(DirType::Mixed, PlatformContext::None, ""));
        assert_eq!(out.playlists[0].member_paths, vec![disc.clone()]);
        assert!(out.playlists[0].missing_members.is_empty());
        assert_eq!(candidate_paths(&out), vec![disc]);
    }

    #[test]
    fn forced_platform_narrows_extensions_hint_does_not() {
        let scratch = ScratchDir::new("forced_vs_hint");
        let gba = scratch.file("a.gba", b"x");
        let md = scratch.file("b.md", b"x"); // a stray Genesis cart in a folder labeled GBA

        let forced = classify_directory(scratch.path(), &ctx(DirType::Mixed, PlatformContext::Forced(Platform::GameBoyAdvance), ""));
        assert_eq!(candidate_paths(&forced), vec![gba.clone()]);
        assert_eq!(forced.unknown, vec![md.clone()]);

        let hinted = classify_directory(scratch.path(), &ctx(DirType::Mixed, PlatformContext::Hint(Platform::GameBoyAdvance), ""));
        assert_eq!(candidate_paths(&hinted), vec![gba, md], "a hint must never make a real file disappear");
    }

    #[test]
    fn declared_file_shapes_and_a_disabled_extension_heuristic_widen_acceptance() {
        let scratch = ScratchDir::new("widen");
        let msx = scratch.file("a.msx", b"x"); // no platform's extension table lists .msx
        let bare = scratch.file("noextension", b"x");

        let default = classify_directory(scratch.path(), &ctx(DirType::Mixed, PlatformContext::None, ""));
        assert!(default.rom_candidates.is_empty());

        let declared = classify_directory(scratch.path(), &ctx(DirType::Mixed, PlatformContext::None, "msx, noext"));
        assert_eq!(candidate_paths(&declared), vec![msx.clone(), bare.clone()]);

        let mut off = ctx(DirType::Mixed, PlatformContext::None, "");
        off.heuristics = HeuristicFlags::new(false, true);
        assert_eq!(candidate_paths(&classify_directory(scratch.path(), &off)), vec![msx, bare]);
    }

    #[test]
    fn a_bios_directorys_unidentified_files_are_unknown_not_roms() {
        // Synthetic bytes match nothing in biosdb.bin, so nothing is identified -- and in a
        // BIOS-only directory an unidentified file still isn't a game.
        let scratch = ScratchDir::new("bios_dir");
        let file = scratch.file("system.bin", b"not a real bios");
        let out = classify_directory(scratch.path(), &ctx(DirType::Bios, PlatformContext::None, ""));
        assert!(out.rom_candidates.is_empty() && out.system_files.is_empty());
        assert_eq!(out.unknown, vec![file]);
    }

    #[test]
    fn a_rom_directorys_unidentified_small_files_stay_candidates() {
        let scratch = ScratchDir::new("rom_dir");
        let file = scratch.file("game.gba", b"not a real bios");
        let out = classify_directory(scratch.path(), &ctx(DirType::Rom, PlatformContext::Forced(Platform::GameBoyAdvance), ""));
        assert!(out.system_files.is_empty());
        assert_eq!(candidate_paths(&out), vec![file]);
    }

    // -- bundles -------------------------------------------------------------

    #[test]
    fn a_leaf_matching_a_declared_dir_shape_is_one_bundle() {
        let scratch = ScratchDir::new("bundle_leaf");
        scratch.file("game/a.rom", b"x");
        scratch.file("game/b.rom", b"x");
        let game = scratch.path().join("game");

        let bundle = bundle_at(&game, scratch.path(), &ctx(DirType::Mixed, PlatformContext::None, "dir(rom)"));
        assert_eq!(bundle, Some(Bundle { path: game, platform_hint: None }));
    }

    #[test]
    fn a_disk_folder_is_its_own_bundle_and_its_parent_is_never_climbed_to() {
        // game/disk1/, game/disk2/ each resolve independently -- game/ itself is never
        // tested or collapsed into one candidate; linking them into one game is a
        // database-level concern now, confirmed by the user, not this scan's.
        let scratch = ScratchDir::new("bundle_disks");
        scratch.file("game/disk1/a.raw", b"x");
        scratch.file("game/disk2/b.raw", b"x");
        let disk1 = scratch.path().join("game").join("disk1");
        let disk2 = scratch.path().join("game").join("disk2");

        let shapes = ctx(DirType::Mixed, PlatformContext::None, "dir(raw)");
        assert_eq!(bundle_at(&disk1, scratch.path(), &shapes).map(|b| b.path), Some(disk1));
        assert_eq!(bundle_at(&disk2, scratch.path(), &shapes).map(|b| b.path), Some(disk2));
        // Not declared as a shape itself (only its children are) and not a leaf besides --
        // game/ never qualifies.
        assert_eq!(bundle_at(&scratch.path().join("game"), scratch.path(), &shapes), None);
    }

    #[test]
    fn the_boundary_is_never_a_bundle_even_as_a_leaf() {
        // A platform folder where every game sits flat (no subdirectories at all) is still
        // a leaf by is_leaf's definition -- it must not become one bundle regardless.
        let scratch = ScratchDir::new("bundle_boundary");
        scratch.file("a.rom", b"x");
        scratch.file("b.rom", b"x");
        assert_eq!(bundle_at(scratch.path(), scratch.path(), &ctx(DirType::Mixed, PlatformContext::None, "dir(rom)")), None);
    }

    #[test]
    fn a_signature_check_that_finds_nothing_confirms_nothing() {
        let scratch = ScratchDir::new("bundle_signature_negative");
        scratch.file("game/data.bin", b"no signature in here");
        let game = scratch.path().join("game");
        let forced = ctx(DirType::Mixed, PlatformContext::Forced(Platform::Msx), "");
        assert_eq!(bundle_at(&game, scratch.path(), &forced), None);
    }

    /// A DSi CDN dump as collections hold it: a folder per game with its content, a TMD whose
    /// title id says DSiWare, the title key and password -- under a folder filed as DSi. It is
    /// one game (each file was a game of its own, the content apart from its TMD).
    #[test]
    fn a_dsi_cdn_folder_is_one_game_though_its_tmd_says_dsiware() {
        let scratch = ScratchDir::new("bundle_dsi_cdn");
        let mut tmd = vec![0u8; 0x208];
        tmd[0..4].copy_from_slice(&0x0001_0001u32.to_be_bytes());
        tmd[0x140 + 0x4C..0x140 + 0x54].copy_from_slice(&0x0003_0004_4B48_4F45u64.to_be_bytes());
        tmd[0x140 + 0x9E..0x140 + 0xA0].copy_from_slice(&1u16.to_be_bytes());
        scratch.file("Hoge Quest (Europe)/tmd.1", &tmd);
        scratch.file("Hoge Quest (Europe)/00000001", &[0u8; 64]);
        scratch.file("Hoge Quest (Europe)/titleKey.bin", &[1u8; 16]);
        scratch.file("Hoge Quest (Europe)/titlePass.txt", b"mypass");
        let game = scratch.path().join("Hoge Quest (Europe)");
        let dsi = ctx(DirType::Mixed, PlatformContext::Hint(Platform::NintendoDsi), "");
        assert_eq!(bundle_at(&game, scratch.path(), &dsi), Some(Bundle { path: game.clone(), platform_hint: Some(Platform::NintendoDsi) }));
        let dsiware = ctx(DirType::Mixed, PlatformContext::Hint(Platform::DsiWare), "");
        assert_eq!(bundle_at(&game, scratch.path(), &dsiware).map(|b| b.platform_hint), Some(Some(Platform::DsiWare)));
        // Another console's TMD is still not one.
        let ctr = ctx(DirType::Mixed, PlatformContext::Hint(Platform::Nintendo3ds), "");
        assert_eq!(bundle_at(&game, scratch.path(), &ctr), None);
    }

    /// A Wii CDN download as collections hold it: the contents `00000000`, `00000001`, ...,
    /// a TMD per version and the ticket. One game, where each file was a game of its own.
    #[test]
    fn a_wii_cdn_folder_is_one_game() {
        let scratch = ScratchDir::new("bundle_wii_cdn");
        let mut tmd = vec![0u8; 0x208];
        tmd[0..4].copy_from_slice(&0x0001_0001u32.to_be_bytes());
        tmd[0x140 + 0x4C..0x140 + 0x54].copy_from_slice(&0x0001_0001_4841_4B45u64.to_be_bytes());
        tmd[0x140 + 0x9E..0x140 + 0xA0].copy_from_slice(&2u16.to_be_bytes());
        scratch.file("Hoge Channel (Japan)/tmd.3", &tmd);
        scratch.file("Hoge Channel (Japan)/00000000", &[0u8; 64]);
        scratch.file("Hoge Channel (Japan)/00000001", &[1u8; 64]);
        scratch.file("Hoge Channel (Japan)/cetk", &[2u8; 64]);
        let game = scratch.path().join("Hoge Channel (Japan)");
        let wii = ctx(DirType::Mixed, PlatformContext::Hint(Platform::Wii), "");
        assert_eq!(bundle_at(&game, scratch.path(), &wii), Some(Bundle { path: game.clone(), platform_hint: Some(Platform::Wii) }));
        // Under a Wii U folder too (its CDN holds vWii titles), but never under a DSi one.
        let wiiu = ctx(DirType::Mixed, PlatformContext::Hint(Platform::WiiU), "");
        assert_eq!(bundle_at(&game, scratch.path(), &wiiu).map(|b| b.platform_hint), Some(Some(Platform::WiiU)));
        let dsi = ctx(DirType::Mixed, PlatformContext::Hint(Platform::NintendoDsi), "");
        assert_eq!(bundle_at(&game, scratch.path(), &dsi), None);
    }

    // -- Layer 2: walk/scan -----------------------------------------------------

    #[test]
    fn a_flat_directory_has_no_bundles() {
        let scratch = ScratchDir::new("walk_flat");
        let a = scratch.file("a.gba", b"x");
        let b = scratch.file("b.gba", b"x");
        let mut out = WalkResult::default();
        walk(scratch.path(), scratch.path(), &ctx(DirType::Mixed, PlatformContext::None, ""), true, &mut out);
        let mut paths: Vec<_> = out.rom_candidates.iter().map(|c| c.path.clone()).collect();
        paths.sort();
        assert_eq!(paths, vec![a, b]);
    }

    #[test]
    fn mycom_disk_folders_each_become_a_candidate_the_parent_never_does() {
        let scratch = ScratchDir::new("walk_mycom");
        scratch.file("game/disk1/a.raw", b"x");
        scratch.file("game/disk2/b.raw", b"x");
        let disk1 = scratch.path().join("game").join("disk1");
        let disk2 = scratch.path().join("game").join("disk2");

        let mut out = WalkResult::default();
        walk(scratch.path(), scratch.path(), &ctx(DirType::Mixed, PlatformContext::None, "dir(raw)"), true, &mut out);
        let mut paths: Vec<_> = out.rom_candidates.iter().map(|c| c.path.clone()).collect();
        paths.sort();
        assert_eq!(paths, {
            let mut expected = vec![disk1, disk2];
            expected.sort();
            expected
        });
        assert!(out.rom_candidates.iter().all(|c| c.kind == CandidateKind::DirectoryBundle));
    }

    #[test]
    fn separated_by_subdir_resolves_each_platform_folder_independently() {
        // Confirms the swallow guard end to end: several loose MSX-signature-bearing
        // directories under one platform folder each resolve on their own; the platform
        // folder itself is never tested (it's the boundary for everything under it).
        let scratch = ScratchDir::new("walk_separated");
        // A plausible (if synthetic) MSX cassette header -- CAS_HEADER from mycom.rs.
        const CAS_HEADER: [u8; 8] = [0x1F, 0xA6, 0xDE, 0xBA, 0xCC, 0x13, 0x7D, 0x74];
        scratch.file("msx/GameA/data.cas", &CAS_HEADER);
        scratch.file("msx/GameB/data.cas", &CAS_HEADER);
        let game_a = scratch.path().join("msx").join("GameA");
        let game_b = scratch.path().join("msx").join("GameB");

        let mut known = std::collections::HashMap::new();
        known.insert("msx".to_string(), Platform::Msx);
        let layout = PlatformLayout::SeparatedBySubdir(known);
        let base = ctx(DirType::Mixed, PlatformContext::None, "");
        let out = scan(scratch.path(), &layout, &base, true);

        let mut paths: Vec<_> = out.rom_candidates.iter().map(|c| c.path.clone()).collect();
        paths.sort();
        assert_eq!(paths, {
            let mut expected = vec![game_a, game_b];
            expected.sort();
            expected
        });
        assert!(out.rom_candidates.iter().all(|c| c.platform_hint == Some(Platform::Msx)), "{:?}", out.rom_candidates);
    }

    /// A game folder straight in a root of platform folders, with no platform of its own
    /// known: one game, not a platform folder of loose files.
    #[test]
    fn a_game_folder_in_a_root_without_a_platform_is_one_bundle() {
        let scratch = ScratchDir::new("walk_root_game_folder");
        let mut sfo = Vec::new();
        sfo.extend_from_slice(&Sfo::MAGIC);
        sfo.extend_from_slice(&0x0101u32.to_le_bytes());
        sfo.extend_from_slice(&0x24u32.to_le_bytes()); // key table
        sfo.extend_from_slice(&0x30u32.to_le_bytes()); // data table
        sfo.extend_from_slice(&1u32.to_le_bytes());
        sfo.extend_from_slice(&0u16.to_le_bytes());
        sfo.extend_from_slice(&0x0204u16.to_le_bytes());
        sfo.extend_from_slice(&10u32.to_le_bytes());
        sfo.extend_from_slice(&16u32.to_le_bytes());
        sfo.extend_from_slice(&0u32.to_le_bytes());
        sfo.extend_from_slice(b"TITLE_ID\0");
        sfo.resize(0x30, 0);
        sfo.extend_from_slice(b"BLUS00001\0");
        scratch.file("Some Game/PS3_GAME/PARAM.SFO", &sfo);
        scratch.file("Some Game/PS3_GAME/USRDIR/EBOOT.BIN", b"x");
        let out = scan(scratch.path(), &PlatformLayout::SeparatedBySubdir(Default::default()), &ctx(DirType::Mixed, PlatformContext::None, ""), true);
        assert_eq!(candidate_paths_walk(&out), vec![scratch.path().join("Some Game")]);
        assert_eq!(out.rom_candidates[0].kind, CandidateKind::DirectoryBundle);
        assert_eq!(out.rom_candidates[0].platform_hint, Some(Platform::PlayStation3));
    }

    /// A directory configured below the root is walked with its own settings: here its
    /// forced platform narrows what it takes, as a scan of it alone would.
    #[test]
    fn a_configured_subfolder_keeps_its_own_settings() {
        let scratch = ScratchDir::new("walk_configured_child");
        let loose = scratch.file("b.md", b"x");
        let inside = scratch.file("gba/b.md", b"x");
        let gba = scratch.file("gba/a.gba", b"x");
        let mut base = ctx(DirType::Mixed, PlatformContext::None, "");
        base.configured = std::sync::Arc::new(vec![ConfiguredDir { path: scratch.path().join("gba"), dir_type: DirType::Mixed, platform: Some(Platform::GameBoyAdvance) }]);
        let mut out = WalkResult::default();
        walk(scratch.path(), scratch.path(), &base, true, &mut out);
        assert_eq!(candidate_paths_walk(&out), vec![loose, gba]);
        assert_eq!(out.unknown, vec![inside]);
    }

    /// A sheet naming tracks in another folder (one up, one down): the tracks are the
    /// sheet's, whichever folder the walk reaches first.
    #[test]
    fn tracks_in_other_folders_are_the_sheets() {
        let scratch = ScratchDir::new("walk_cross_dir_tracks");
        scratch.file("shared.bin", b"x");
        scratch.file("game/Tracks/track 02.bin", b"x");
        let cue = scratch.file("game/game.cue", b"FILE \"..\\shared.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\nFILE \"Tracks\\track 02.bin\" BINARY\n  TRACK 02 AUDIO\n    INDEX 01 00:00:00\n");
        let mut out = WalkResult::default();
        walk(scratch.path(), scratch.path(), &ctx(DirType::Mixed, PlatformContext::None, ""), false, &mut out);
        assert_eq!(candidate_paths_walk(&out), vec![cue]);
    }

    /// A link to a sibling folder lists the same files again: walked once, under the real
    /// folder's path.
    #[cfg(unix)]
    #[test]
    fn a_link_to_a_sibling_is_walked_once() {
        let scratch = ScratchDir::new("walk_sibling_link");
        let real = scratch.file("b/game.gba", b"x");
        std::os::unix::fs::symlink(scratch.path().join("b"), scratch.path().join("a")).unwrap();
        let mut out = WalkResult::default();
        walk(scratch.path(), scratch.path(), &ctx(DirType::Mixed, PlatformContext::None, ""), false, &mut out);
        assert_eq!(candidate_paths_walk(&out), vec![real]);
    }

    #[test]
    fn skip_unknown_controls_whether_unaccepted_files_are_still_candidates() {
        let scratch = ScratchDir::new("walk_skip_unknown");
        let odd = scratch.file("mystery.xyz", b"x");
        let base = ctx(DirType::Mixed, PlatformContext::None, "");

        let mut skipped = WalkResult::default();
        walk(scratch.path(), scratch.path(), &base, true, &mut skipped);
        assert!(skipped.rom_candidates.is_empty());
        assert_eq!(skipped.unknown, vec![odd.clone()]);

        let mut kept = WalkResult::default();
        walk(scratch.path(), scratch.path(), &base, false, &mut kept);
        assert!(kept.unknown.is_empty());
        assert_eq!(kept.rom_candidates.len(), 1);
        assert_eq!(kept.rom_candidates[0].kind, CandidateKind::Unknown);
        assert_eq!(kept.rom_candidates[0].path, odd);
    }

    #[test]
    fn unknown_archive_args_forces_off_only_the_platform_driven_targets() {
        let base = super::super::models::ArchiveArgs {
            do_zip: true,
            do_7z: true,
            do_chd: true,
            do_rvz: true,
            do_gcz: true,
            do_wia: true,
            ..Default::default()
        };
        let unknown = unknown_archive_args(&base);
        assert!(unknown.do_zip && unknown.do_7z, "zip/7z need no platform, kept as-is");
        assert!(!unknown.do_chd && !unknown.do_rvz && !unknown.do_gcz && !unknown.do_wia);
    }
}

//! Checking a copy whose bytes are new, and giving it an identity of its own.
//!
//! The ordinary check compares a copied file's own hash against what the catalogue records
//! for it (`super::check_file`). That only works when the bytes are unchanged. The moment a
//! copy re-containers -- a cue+bin written as one CHD, a 7z written as a zip, an archive
//! unpacked to loose files -- the destination's bytes are new and there is nothing recorded
//! to compare them to. Comparing the file would say "different", which is true and useless.
//!
//! What is preserved across re-containering is the **payload**, and the catalogue already
//! describes it: `content_contents` links an archive's content row to one content row per
//! member. So the check decodes the destination, hashes each member off that same pass, and
//! requires every member the source had to come back with matching bytes. That is a stronger
//! statement than the whole-file check, not a weaker one: it says the games inside survived,
//! which is the thing anyone actually cares about.
//!
//! Then it gives the new container an identity: its own `content` row with the full hash set
//! a scan would compute, linked to the same members. Two reasons, both load-bearing. The next
//! integrity check becomes a cheap whole-file comparison instead of another decode. And the
//! new archive is now something a DAT can match, which it could not be while it borrowed the
//! source's content row -- a row describing different bytes.

use std::collections::HashMap;
use std::path::Path;

use sqlx::Row;

use crate::common::models::Platform;
use crate::database::content;
use crate::rom::hash::{FileHashRecord, HashFlags, HashReport};
use crate::rom::operation::CheckedOutput;

use super::super::{App, Error, Result};

/// What decoding a re-containered copy concluded.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentOutcome {
    /// Every member the source had came back with matching bytes.
    Matched {
        members: usize,
        /// The destination's own new `content` row.
        content_id: u64,
        /// It got the full hash set, so the next check need not decode it again.
        hashed: bool,
    },
    /// At least one member is missing from the output or came back different. Named so the
    /// summary can say which game inside the archive is wrong, not just which file.
    Mismatched { missing: Vec<String> },
    /// Nothing recorded to check against: the source content has neither member rows nor a
    /// hash of its own, so the payload that went in was never described. Not a failure --
    /// the same "no claim either way" `super::CheckOutcome::Unverifiable` makes.
    NoMembers,
}

/// What the payload was, as the catalogue records it: one entry per thing that has to come
/// back out of the destination.
///
/// Usually that is `parent`'s `content_contents` rows -- the members of the archive that was
/// copied. When `parent` has none it *is* the payload: a loose ROM written into a new archive
/// went in as one file and has to come back as one, so the content row itself becomes the
/// single expected member. That is what makes this check work in both directions, and it is
/// why a raw-to-archive copy is checked rather than waved through.
async fn expected_members(app: &App, parent: u64) -> Result<Vec<(u64, String, HashMap<String, String>)>> {
    let rows = sqlx::query(
        "SELECT cc.child_content_id, COALESCE(cc.member_path, c.file_name, '') AS member_path \
         FROM content_contents cc JOIN content c ON c.id = cc.child_content_id \
         WHERE cc.parent_content_id = ?",
    )
    .bind(parent)
    .fetch_all(&app.pool)
    .await?;
    let mut out = Vec::new();
    for row in &rows {
        let child: u64 = row.try_get("child_content_id")?;
        let path: String = row.try_get("member_path")?;
        let hashes: Vec<(String, String)> = sqlx::query_as("SELECT algorithm, value_hex FROM hashes WHERE content_id = ?").bind(child).fetch_all(&app.pool).await?;
        out.push((child, path, hashes.into_iter().collect()));
    }
    if out.is_empty() {
        let name: Option<String> = sqlx::query_scalar("SELECT file_name FROM content WHERE id = ?").bind(parent).fetch_optional(&app.pool).await?.flatten();
        let hashes: Vec<(String, String)> = sqlx::query_as("SELECT algorithm, value_hex FROM hashes WHERE content_id = ?").bind(parent).fetch_all(&app.pool).await?;
        if !hashes.is_empty() {
            out.push((parent, name.unwrap_or_default(), hashes.into_iter().collect()));
        }
    }
    Ok(out)
}

/// The container's own whole-file hashes, shaped as a member record.
///
/// The destination of an *extracting* copy is a loose file, so it has no `content_hashes` of
/// its own -- it is itself the one member the source archive held. Comparing the whole-file
/// report is the same question asked of the same bytes.
fn whole_as_member(path: &Path, report: &HashReport) -> crate::rom::hash::FileHashRecord {
    crate::rom::hash::FileHashRecord {
        path: path.display().to_string(),
        size_bytes: std::fs::metadata(path).map(|m| m.len()).ok(),
        crc32: report.crc32.clone(),
        xxh3_128: report.xxh3_128.clone(),
        md5: report.md5.clone(),
        sha1: report.sha1.clone(),
        sha256: report.sha256.clone(),
        ..Default::default()
    }
}

/// Whether a member's freshly computed hashes agree with one the catalogue recorded.
///
/// Any algorithm they have in common has to match; if they have none in common the member
/// cannot be confirmed and counts as missing, rather than being waved through.
fn agrees(recorded: &HashMap<String, String>, got: &crate::rom::hash::FileHashRecord) -> bool {
    let mine = [("crc32", &got.crc32), ("xxh3_128", &got.xxh3_128), ("md5", &got.md5), ("sha1", &got.sha1), ("sha256", &got.sha256)];
    let mut compared = false;
    for (algo, value) in mine {
        let (Some(value), Some(expected)) = (value.as_deref(), recorded.get(algo)) else { continue };
        if !value.eq_ignore_ascii_case(expected) {
            return false;
        }
        compared = true;
    }
    compared
}

/// The container's members and the container's own identity -- the hash of its raw bytes,
/// which is what a scan records for the same file -- from one `Single` pass: the raw read on
/// one thread, the decode of the members on another, at the same time.
///
/// Not the decoded report's top-level hashes. Those are filled in only for a single-member
/// archive, where they are that *member's* -- so a CD CHD (a cue sheet plus at least one
/// track) had none and every verified CD-to-CHD copy failed here, while a DVD CHD was
/// recorded under its decoded image's hash, an identity the next scan of the same file
/// would not have found.
async fn decode_and_hash(app: &App, path: &Path, flags: HashFlags) -> Result<(Vec<FileHashRecord>, HashReport)> {
    use crate::rom::operation::{Detect, Single, SingleArgs, StageSwitches};
    let style = super::super::sheets::style_setting(app).await?;
    // An RA hash needs a console id and is a rom-level value anyway, not the file's.
    let flags = flags.difference(HashFlags::RAHASH);
    let args = SingleArgs {
        stages: StageSwitches { hash: true, ..StageSwitches::NONE },
        hash_flags: flags,
        detect: Detect::Trust(Platform::Unknown),
        whole_file: true,
        container_hash: Some(flags),
        ..SingleArgs::default()
    }
    .with_sheet_style(style);
    let owned = path.to_path_buf();
    let outcome = tokio::task::spawn_blocking(move || Single::new(owned, args).run(&mut |_| true))
        .await
        .map_err(|e| Error::internal(e.to_string()))?
        .map_err(|e| Error::internal(format!("{}: {e}", path.display())))?;
    let raw = match outcome.input_container {
        Some(Ok(raw)) => raw,
        Some(Err(e)) => return Err(Error::internal(format!("{}: {e}", path.display()))),
        None => return Err(Error::internal(format!("{}: not hashed", path.display()))),
    };
    // A loose file's one content record is itself: it has no members.
    let members = if outcome.content_files.iter().all(|f| f.source_path.is_some()) { Vec::new() } else { outcome.input_hashes.map(|r| r.content_hashes).unwrap_or_default() };
    Ok((members, raw))
}

/// Decodes `path`, checks every member the source's content links to, and -- when they all
/// match -- records the new container as content of its own.
///
/// `flags` is the scan's own hash set, so the new archive is described exactly as a scanned
/// one would be and a DAT can match it.
///
/// `read_back` is what the conversion already read back of `path` (`Single`'s output check);
/// without it, or without members in it, the file is read here.
pub async fn check(app: &App, path: &Path, source_content_id: u64, platform: Platform, flags: HashFlags, read_back: Option<&CheckedOutput>) -> Result<ContentOutcome> {
    let expected = expected_members(app, source_content_id).await?;
    if expected.is_empty() {
        return Ok(ContentOutcome::NoMembers);
    }

    let (decoded, raw) = match read_back {
        Some(CheckedOutput { container: Ok(raw), members: Some(Ok(members)), .. }) => (members.clone(), raw.clone()),
        // A loose output (an extraction) is read back without members: it has none.
        Some(CheckedOutput { container: Ok(raw), members: None, .. }) => (Vec::new(), raw.clone()),
        _ => decode_and_hash(app, path, flags).await?,
    };

    // An archive answers with one record per member; a loose file answers with itself.
    let is_container = !decoded.is_empty();
    let members: Vec<FileHashRecord> = if is_container { decoded } else { vec![whole_as_member(path, &raw)] };
    let mut missing = Vec::new();
    for (_, member_path, recorded) in &expected {
        if !members.iter().any(|got| agrees(recorded, got)) {
            missing.push(if member_path.is_empty() { "(unnamed member)".to_string() } else { member_path.clone() });
        }
    }
    if !missing.is_empty() {
        return Ok(ContentOutcome::Mismatched { missing });
    }

    // Only now: the payload survived, so the container is worth recording. Its own bytes are
    // new, so this is a new content row -- borrowing the source's would have it describe
    // bytes that are not there.
    let name = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let whole = raw;
    if whole.has_no_fallback() {
        return Err(Error::internal(format!("{}: decoded, but produced no whole-file hash", path.display())));
    }
    let (content_id, existed) = content::upsert_content(&app.pool, platform, &name, size, &whole).await?;
    // Members only for a real container. An extracted file is not the parent of the content
    // it used to sit inside -- it *is* that content -- so linking it to itself would make the
    // junction claim an archive that does not exist.
    if is_container {
        for (child, member_path, _) in &expected {
            content::link_member(&app.pool, content_id, *child, (!member_path.is_empty()).then_some(member_path.as_str())).await?;
        }
    }
    Ok(ContentOutcome::Matched { members: expected.len(), content_id, hashed: !existed })
}

/// Hashes a file whose payload is new and records it as content of its own.
///
/// This is the third kind of check, and the only one that concludes nothing. When a copy
/// converts the *payload* -- an iNES header added or stripped, an N64 image byte-swapped, a
/// 3DS/NDS image decrypted -- the bytes inside are deliberately different, so neither the
/// whole-file comparison nor the member comparison applies: there is nothing they should
/// equal. What the new file needs is an identity, computed the way a scan would compute it,
/// so a DAT can say what it now is. The caller follows it with a `hash_match`, which is the
/// "match to catalog" half.
///
/// Payload conversion is switched off in this copy path today (`StageSwitches { convert:
/// false }`, which `run::carry_ra_hashes` depends on), so nothing reaches this yet. It is
/// written because the day that switch flips, carrying hashes across would be wrong in a way
/// no test would catch -- the file would simply be recorded as something it is not.
pub async fn rehash(app: &App, path: &Path, platform: Platform, flags: HashFlags) -> Result<u64> {
    let (members, whole) = decode_and_hash(app, path, flags).await?;

    let name = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if whole.has_no_fallback() {
        return Err(Error::internal(format!("{}: hashed, but produced no whole-file hash", path.display())));
    }
    let (content_id, _) = content::upsert_content(&app.pool, platform, &name, size, &whole).await?;
    // Members of the new container, where there are any: the payload changed, so these are
    // new contents too, and they are what a member-level match will look at.
    for member in &members {
        let member_name = Path::new(&member.path).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| member.path.clone());
        let one = HashReport {
            crc32: member.crc32.clone(),
            xxh3_128: member.xxh3_128.clone(),
            md5: member.md5.clone(),
            sha1: member.sha1.clone(),
            sha256: member.sha256.clone(),
            rahash: None,
            content_hashes: Vec::new(),
        };
        if one.has_no_fallback() {
            continue;
        }
        let (child, _) = content::upsert_content(&app.pool, platform, &member_name, member.size_bytes.unwrap_or(0), &one).await?;
        content::link_member(&app.pool, content_id, child, Some(member.path.as_str())).await?;
    }
    Ok(content_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rom::hash::FileHashRecord;

    fn recorded(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(a, v)| (a.to_string(), v.to_string())).collect()
    }

    fn got(crc: Option<&str>, sha1: Option<&str>) -> FileHashRecord {
        FileHashRecord { path: "m".into(), size_bytes: Some(1), crc32: crc.map(str::to_string), sha1: sha1.map(str::to_string), ..Default::default() }
    }

    #[test]
    fn a_member_agrees_when_every_shared_algorithm_matches() {
        assert!(agrees(&recorded(&[("crc32", "aabbccdd")]), &got(Some("aabbccdd"), None)));
        assert!(agrees(&recorded(&[("crc32", "AABBCCDD")]), &got(Some("aabbccdd"), None)), "hex case is not part of the value");
    }

    #[test]
    fn one_disagreeing_algorithm_is_enough_to_fail() {
        assert!(!agrees(&recorded(&[("crc32", "aabbccdd"), ("sha1", "ffff")]), &got(Some("aabbccdd"), Some("eeee"))));
    }

    /// No algorithm in common means the member is unconfirmed, which is not the same as
    /// confirmed -- waving it through would make the whole check meaningless for exactly the
    /// content nobody has hashed properly.
    #[test]
    fn nothing_in_common_does_not_count_as_agreement() {
        assert!(!agrees(&recorded(&[("sha256", "abcd")]), &got(Some("aabbccdd"), None)));
        assert!(!agrees(&HashMap::new(), &got(Some("aabbccdd"), None)));
    }
}

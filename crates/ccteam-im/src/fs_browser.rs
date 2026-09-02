//! Pure directory-listing model for the Telegram `/projects` folder browser
//! (`docs/superpowers/specs/2026-09-02-bot-fs-project-browser-design.md`).
//!
//! No Telegram/gateway knowledge and no I/O beyond `std::fs::read_dir` /
//! `Path::canonicalize` — the caller (gateway) owns per-chat nav state and
//! callback wiring on top of these pure functions, so this module unit-tests
//! standalone against a tempdir.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Entries shown per page in the browser.
pub const PAGE_SIZE: usize = 8;

/// Directory names that dirs matching are skipped entirely
/// (in addition to any name starting with `.`).
const SKIP_NAMES: &[&str] = &["node_modules", "target", "__pycache__"];

/// Hard cap on raw `read_dir` entries scanned per [`list`] call (FS-SEC-4) —
/// bounds worst-case cost (time and memory) of listing a directory with an
/// unreasonable number of children, independent of where the call runs.
const MAX_SCANNED_ENTRIES: usize = 20_000;

/// One directory entry in a [`Page`].
pub struct Entry {
    /// File-system name of the directory (not the full path).
    pub name: String,
    /// `Some(slug)` if this directory's canonical path matches a registered
    /// project's canonical path.
    pub slug: Option<String>,
}

/// One page of a directory listing.
pub struct Page {
    /// Normalized path relative to `root` (no `.` or `..` components).
    pub rel: PathBuf,
    /// Canonical absolute path of the listed directory.
    pub abs: PathBuf,
    /// Directory entries on this page (already paginated and sorted).
    pub entries: Vec<Entry>,
    /// 1-indexed current page number, clamped into `1..=pages`.
    pub page: usize,
    /// Total number of pages, always `>= 1`.
    pub pages: usize,
    /// `Some(slug)` if the *listed* directory itself is a registered project.
    pub current_slug: Option<String>,
}

/// Resolves `root.join(rel)` to a canonical absolute path, refusing to leave
/// `root` (via `..` or a symlink) and requiring the result to be a directory.
pub fn resolve(root: &Path, rel: &Path) -> Result<PathBuf> {
    let root_canon = root
        .canonicalize()
        .with_context(|| format!("root does not exist: {}", root.display()))?;
    let joined = root_canon.join(rel);
    let canon = joined
        .canonicalize()
        .with_context(|| format!("path does not exist or is unreadable: {}", joined.display()))?;
    if !canon.starts_with(&root_canon) {
        bail!("path escapes root: {}", canon.display());
    }
    if !canon.is_dir() {
        bail!("not a directory: {}", canon.display());
    }
    Ok(canon)
}

/// Returns the parent of `rel` (relative to the browser root), or `None` if
/// `rel` already names the root itself.
pub fn parent(rel: &Path) -> Option<PathBuf> {
    let mut comps: Vec<_> = rel.components().collect();
    if comps.is_empty() {
        return None;
    }
    comps.pop();
    Some(comps.iter().collect())
}

fn canonicalize_registered(registered: &[(String, PathBuf)]) -> Vec<(String, PathBuf)> {
    registered
        .iter()
        .filter_map(|(slug, path)| path.canonicalize().ok().map(|abs| (slug.clone(), abs)))
        .collect()
}

fn find_slug<'a>(registered_canon: &'a [(String, PathBuf)], abs: &Path) -> Option<&'a str> {
    registered_canon
        .iter()
        .find(|(_, p)| p == abs)
        .map(|(slug, _)| slug.as_str())
}

/// Lists directories under `root/rel`, filtered, sorted (registered projects
/// first, then case-insensitive by name), and paginated at [`PAGE_SIZE`].
///
/// `page` is 1-indexed and clamped into `1..=pages`.
pub fn list(
    root: &Path,
    rel: &Path,
    registered: &[(String, PathBuf)],
    page: usize,
) -> Result<Page> {
    list_capped(root, rel, registered, page, MAX_SCANNED_ENTRIES)
}

/// [`list`], with the scan cap as a parameter — split out so a test can
/// exercise the cap without creating tens of thousands of directories.
fn list_capped(
    root: &Path,
    rel: &Path,
    registered: &[(String, PathBuf)],
    page: usize,
    max_scanned_entries: usize,
) -> Result<Page> {
    let root_canon = root
        .canonicalize()
        .with_context(|| format!("root does not exist: {}", root.display()))?;
    let abs = resolve(root, rel)?;
    let rel_out = abs
        .strip_prefix(&root_canon)
        .map(Path::to_path_buf)
        .unwrap_or_default();

    let registered_canon = canonicalize_registered(registered);
    let current_slug = find_slug(&registered_canon, &abs).map(str::to_owned);

    let read_dir = std::fs::read_dir(&abs)
        .with_context(|| format!("cannot read directory: {}", abs.display()))?;

    let mut entries: Vec<Entry> = Vec::new();
    for (scanned, dir_entry) in read_dir.enumerate() {
        if scanned >= max_scanned_entries {
            bail!(
                "directory has too many entries to list (>{max_scanned_entries}): {}",
                abs.display()
            );
        }
        let dir_entry = dir_entry
            .with_context(|| format!("cannot read directory entry in {}", abs.display()))?;
        let name = dir_entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || SKIP_NAMES.contains(&name.as_str()) {
            continue;
        }
        let path = dir_entry.path();
        // Follow symlinks for the dir check (a symlinked dir should still show up).
        if !path.is_dir() {
            continue;
        }
        let slug = path
            .canonicalize()
            .ok()
            .and_then(|canon_path| find_slug(&registered_canon, &canon_path))
            .map(str::to_owned);
        entries.push(Entry { name, slug });
    }

    entries.sort_by(|a, b| {
        let a_key = (a.slug.is_none(), a.name.to_lowercase());
        let b_key = (b.slug.is_none(), b.name.to_lowercase());
        a_key.cmp(&b_key)
    });

    let total = entries.len();
    let pages = total.div_ceil(PAGE_SIZE).max(1);
    let page = page.clamp(1, pages);
    let start = (page - 1) * PAGE_SIZE;
    let page_entries = entries.into_iter().skip(start).take(PAGE_SIZE).collect();

    Ok(Page {
        rel: rel_out,
        abs,
        entries: page_entries,
        page,
        pages,
        current_slug,
    })
}

/// Short (4 hex char) fingerprint of one rendered page's `rel` + entry names,
/// in order — embedded in `nav:fs:i:<n>:<fp>`/`nav:fs:pick:<n>:<fp>` callback
/// data (FS-SEC-2/F7/F8) so a tap is validated against the exact content it
/// was rendered from, not just an index into whatever the chat's nav cursor
/// happens to point at when the tap resolves. Not cryptographic — a 16-bit
/// FNV-1a — collisions only weaken the staleness check to "acts on the wrong
/// entry anyway", the same risk the unguarded index already carried, so this
/// is defense-in-depth rather than a security boundary in itself.
pub fn fingerprint(rel: &Path, entries: &[Entry]) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= u32::from(b);
            hash = hash.wrapping_mul(0x0100_0193);
        }
    };
    feed(rel.to_string_lossy().as_bytes());
    feed(b"\0");
    for entry in entries {
        feed(entry.name.as_bytes());
        feed(b"\0");
    }
    format!("{:04x}", hash & 0xffff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    fn mkdir(root: &Path, name: &str) -> PathBuf {
        let p = root.join(name);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn filters_hidden_and_service_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        mkdir(root, "visible");
        mkdir(root, ".hidden");
        mkdir(root, "node_modules");
        mkdir(root, "target");
        mkdir(root, "__pycache__");

        let page = list(root, Path::new(""), &[], 1).unwrap();
        let names: Vec<_> = page.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["visible"]);
    }

    #[test]
    fn only_lists_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        mkdir(root, "adir");
        fs::write(root.join("afile.txt"), b"x").unwrap();

        let page = list(root, Path::new(""), &[], 1).unwrap();
        let names: Vec<_> = page.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["adir"]);
    }

    #[test]
    fn marks_registered_by_canonical_path_through_symlinked_root() {
        let tmp = tempfile::tempdir().unwrap();
        let real_root = tmp.path().join("real");
        fs::create_dir_all(&real_root).unwrap();
        let project_dir = mkdir(&real_root, "proj");
        let other_dir = mkdir(&real_root, "other");

        let link_root = tmp.path().join("link");
        symlink(&real_root, &link_root).unwrap();

        // registered path given as the real (non-symlinked) path.
        let registered = vec![("proj-slug".to_string(), project_dir.clone())];

        let page = list(&link_root, Path::new(""), &registered, 1).unwrap();
        let mut by_name: Vec<_> = page
            .entries
            .iter()
            .map(|e| (e.name.as_str(), e.slug.as_deref()))
            .collect();
        by_name.sort();
        assert_eq!(by_name, vec![("other", None), ("proj", Some("proj-slug"))]);

        // The listed directory itself (link_root, canonically == real_root)
        // is not registered, so current_slug is None here...
        assert_eq!(page.current_slug, None);

        // ...but listing the project dir itself surfaces current_slug.
        let inner = list(&link_root, Path::new("proj"), &registered, 1).unwrap();
        assert_eq!(inner.current_slug.as_deref(), Some("proj-slug"));
        let _ = other_dir;
    }

    #[test]
    fn sorts_registered_first_then_case_insensitive_name() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let zeta = mkdir(root, "zeta");
        mkdir(root, "Alpha");
        mkdir(root, "beta");
        let registered = vec![("zeta-slug".to_string(), zeta)];

        let page = list(root, Path::new(""), &registered, 1).unwrap();
        let names: Vec<_> = page.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["zeta", "Alpha", "beta"]);
    }

    #[test]
    fn fingerprint_changes_when_entries_change() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        mkdir(root, "a");
        mkdir(root, "b");
        let page = list(root, Path::new(""), &[], 1).unwrap();
        let fp1 = fingerprint(&page.rel, &page.entries);

        mkdir(root, "c");
        let page2 = list(root, Path::new(""), &[], 1).unwrap();
        let fp2 = fingerprint(&page2.rel, &page2.entries);

        assert_ne!(fp1, fp2, "adding an entry must change the fingerprint");
    }

    #[test]
    fn fingerprint_is_stable_for_identical_input() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        mkdir(root, "a");
        mkdir(root, "b");
        let page1 = list(root, Path::new(""), &[], 1).unwrap();
        let page2 = list(root, Path::new(""), &[], 1).unwrap();
        assert_eq!(
            fingerprint(&page1.rel, &page1.entries),
            fingerprint(&page2.rel, &page2.entries)
        );
    }

    #[test]
    fn fingerprint_differs_across_directories_with_same_entry_names() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        mkdir(root, "x");
        mkdir(&root.join("x"), "same");
        mkdir(root, "y");
        mkdir(&root.join("y"), "same");
        let px = list(root, Path::new("x"), &[], 1).unwrap();
        let py = list(root, Path::new("y"), &[], 1).unwrap();
        assert_ne!(
            fingerprint(&px.rel, &px.entries),
            fingerprint(&py.rel, &py.entries),
            "same entry names in different directories must not collide (rel is mixed in)"
        );
    }

    #[test]
    fn list_rejects_directories_with_too_many_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for i in 0..10 {
            mkdir(root, &format!("d{i:02}"));
        }
        // Cap of 5 against 10 entries — the FS-SEC-4 guard must bail rather
        // than truncate silently.
        let result = list_capped(root, Path::new(""), &[], 1, 5);
        let err = match result {
            Ok(_) => panic!("expected the entry cap to reject this directory"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("too many entries"));

        // A cap that comfortably covers the directory still lists fine.
        let page = list_capped(root, Path::new(""), &[], 1, 100).unwrap();
        assert_eq!(page.pages, 10usize.div_ceil(PAGE_SIZE));
    }

    #[test]
    fn paginates_at_page_size_and_clamps() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for i in 0..17 {
            mkdir(root, &format!("d{i:02}"));
        }

        let page1 = list(root, Path::new(""), &[], 1).unwrap();
        assert_eq!(page1.pages, 3);
        assert_eq!(page1.page, 1);
        assert_eq!(page1.entries.len(), PAGE_SIZE);

        let page3 = list(root, Path::new(""), &[], 3).unwrap();
        assert_eq!(page3.entries.len(), 1);

        let clamped = list(root, Path::new(""), &[], 5).unwrap();
        assert_eq!(clamped.page, 3);
        assert_eq!(clamped.entries.len(), 1);
    }

    #[test]
    fn resolve_rejects_dotdot_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(tmp.path().join("outside")).unwrap();

        let err = resolve(&root, Path::new("../outside")).unwrap_err();
        assert!(err.to_string().contains("escapes root"));
    }

    #[test]
    fn resolve_rejects_symlink_pointing_outside_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("escape")).unwrap();

        let err = resolve(&root, Path::new("escape")).unwrap_err();
        assert!(err.to_string().contains("escapes root"));
    }

    #[test]
    fn resolve_accepts_root_reached_through_a_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let real_root = tmp.path().join("real");
        fs::create_dir_all(&real_root).unwrap();
        mkdir(&real_root, "child");
        let link_root = tmp.path().join("link");
        symlink(&real_root, &link_root).unwrap();

        let abs = resolve(&link_root, Path::new("child")).unwrap();
        assert_eq!(abs, real_root.canonicalize().unwrap().join("child"));
    }

    #[test]
    fn parent_at_root_is_none() {
        assert_eq!(parent(Path::new("")), None);
    }

    #[test]
    fn parent_of_nested_rel_pops_one_level() {
        assert_eq!(parent(Path::new("a/b")), Some(PathBuf::from("a")));
        assert_eq!(parent(Path::new("a")), Some(PathBuf::new()));
    }
}

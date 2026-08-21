//! Conversions between filesystem paths and the `file:` URIs the LSP speaks.
//!
//! On Unix the two differ by little more than a prefix, which is why pasting the path into
//! `file://{path}` worked there. On Windows they differ in nearly every way -- separators, the
//! drive letter's leading slash, percent-encoding -- so both directions need doing properly.

use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use url::Url;

/// The `file:` URI naming `path`, which must be absolute.
pub fn path_to_uri(path: &Path) -> Result<String> {
    // A URI can carry a path that is not valid UTF-8, but rust-analyzer cannot: its own
    // conversion unwraps on one and takes the whole language server down with it.
    if path.to_str().is_none() {
        return Err(anyhow!(
            "{} is not valid UTF-8; rust-analyzer only understands UTF-8 paths",
            path.display()
        ));
    }
    if !path.is_absolute() {
        return Err(anyhow!("{} is not an absolute path", path.display()));
    }

    Url::from_file_path(path)
        .map(|url| url.to_string())
        // Everything absolute has a URI bar the Windows paths that name no drive, such as the
        // `\\?\Volume{...}\` form a volume mounted without a drive letter canonicalizes to.
        .map_err(|()| anyhow!("{} cannot be named by a file URI", path.display()))
}

/// The path `uri` names, or `None` if it is not a `file:` URI naming one.
///
/// Clients do pass URIs where a path is expected, so every path taken from a tool call goes
/// through here first. Note that a Windows path parses as a URI whose scheme is its drive
/// letter, which is why this checks the scheme rather than whether it parsed.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let url = Url::parse(uri).ok()?;
    if url.scheme() != "file" {
        return None;
    }
    url.to_file_path().ok()
}

/// `path` made absolute, in the plainest spelling the platform has for it.
///
/// Canonicalizing is what gets the real spelling of a path whose case or symlinks differ from
/// what the client typed. On Windows it also returns the extended-length `\\?\C:\...` form, which
/// switches off the path parsing everything downstream relies on: `\\?\C:\ws` joined with
/// `src/lib.rs` keeps that forward slash, and the filesystem then rejects the result. `dunce`
/// hands back the plain form whenever the path has one.
pub fn absolute(path: &Path) -> PathBuf {
    if let Ok(canonical) = dunce::canonicalize(path) {
        return canonical;
    }

    // Nothing on disk to canonicalize against; whoever opens the file gets to report that.
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(path)
}

/// `uri` in the spelling used to key documents by.
///
/// The URIs we build and the ones rust-analyzer sends back name the same files but are not
/// spelled the same: on Windows the `url` crate upper-cases the drive letter on the way out of
/// `from_file_path()` while rust-analyzer lower-cases it on the way out of its own conversion,
/// and a client may hand us either, or the `C%3A` and `C|` spellings the LSP specification warns
/// about. Folding all of them together is what makes a stored diagnostic findable again.
pub fn normalize(uri: &str) -> String {
    let Ok(url) = Url::parse(uri) else {
        return uri.to_string();
    };
    if url.scheme() != "file" {
        return uri.to_string();
    }

    let Some((drive, drive_len)) = drive_letter(url.path()) else {
        return url.to_string();
    };

    let mut url = url;
    let normalized = format!(
        "/{}:{}",
        drive.to_ascii_uppercase(),
        &url.path()[drive_len..]
    );
    // Only ever rewrites the drive at the head of the path, so the URI stays valid.
    url.set_path(&normalized);
    url.to_string()
}

/// The drive letter of a `/C:/...`-shaped URI path, and how much of the path it takes up.
///
/// The colon may be percent-encoded, or be the `|` older URIs use in its place.
fn drive_letter(path: &str) -> Option<(char, usize)> {
    let letter = path.strip_prefix('/')?.chars().next()?;
    if !letter.is_ascii_alphabetic() {
        return None;
    }

    let rest = &path[2..];
    let colon_len = if rest.starts_with(':') || rest.starts_with('|') {
        1
    } else if rest.starts_with("%3A") || rest.starts_with("%3a") {
        3
    } else {
        return None;
    };

    // The drive is a whole path segment: `/C:/x` is a drive, `/C:x` is a directory named `C:x`.
    let drive_len = 2 + colon_len;
    match path[drive_len..].chars().next() {
        Some('/') | None => Some((letter, drive_len)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_settles_on_one_spelling_of_the_drive() {
        for uri in [
            "file:///c:/Users/zeenix/src/lib.rs",
            "file:///C:/Users/zeenix/src/lib.rs",
            "file:///c%3A/Users/zeenix/src/lib.rs",
            "file:///C%3a/Users/zeenix/src/lib.rs",
            "file:///c|/Users/zeenix/src/lib.rs",
        ] {
            assert_eq!(
                normalize(uri),
                "file:///C:/Users/zeenix/src/lib.rs",
                "{uri}"
            );
        }

        // A drive is a whole path segment, and a bare one still names the drive's root.
        assert_eq!(normalize("file:///c:"), "file:///C:");
        assert_eq!(normalize("file:///c:/"), "file:///C:/");
    }

    #[test]
    fn normalize_leaves_everything_else_alone() {
        for uri in [
            "file:///home/zeenix/src/lib.rs",
            // Percent-encoding is not decoded: rust-analyzer builds its URIs with the same crate
            // we do, so both sides encode identically.
            "file:///home/zeenix/my%20crate/src/lib.rs",
            // Not a drive letter, just a directory that looks like one.
            "file:///c:x/lib.rs",
            "file:///cc:/lib.rs",
            "file:///1:/lib.rs",
            // A UNC share, whose first segment is a host rather than a drive.
            "file://server/share/lib.rs",
            // Not a file URI, and not a URI at all.
            "untitled:Untitled-1",
            "not a uri",
            "",
        ] {
            assert_eq!(normalize(uri), uri, "{uri}");
        }
    }

    #[test]
    fn normalize_is_idempotent() {
        for uri in ["file:///c%3A/a.rs", "file:///home/zeenix/a.rs", "nonsense"] {
            assert_eq!(normalize(&normalize(uri)), normalize(uri), "{uri}");
        }
    }

    #[test]
    fn relative_paths_have_no_uri() {
        let error = path_to_uri(Path::new("src/lib.rs"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("absolute"), "{error}");
    }

    #[test]
    fn non_file_uris_have_no_path() {
        assert!(uri_to_path("http://example.com/lib.rs").is_none());
        assert!(uri_to_path("not a uri").is_none());
        assert!(uri_to_path("src/lib.rs").is_none());
    }

    #[test]
    fn absolute_leaves_nothing_relative() {
        assert!(absolute(Path::new("src/lib.rs")).is_absolute());
        assert!(absolute(Path::new("no/such/file.rs")).is_absolute());
    }

    #[cfg(unix)]
    #[test]
    fn unix_paths_round_trip() {
        for path in [
            "/home/zeenix/src/lib.rs",
            // The characters a naive `file://{path}` would hand to the URI parser as syntax.
            "/home/zeenix/my crate/src/lib.rs",
            "/home/zeenix/#1?/src/lib.rs",
            "/home/zeenix/ünïcøde/src/lib.rs",
        ] {
            let uri = path_to_uri(Path::new(path)).unwrap();
            assert!(uri.starts_with("file:///"), "{uri}");
            assert_eq!(uri_to_path(&uri).unwrap(), Path::new(path), "{uri}");
            assert_eq!(normalize(&uri), uri, "{uri}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn paths_that_are_not_utf8_are_refused() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let path = PathBuf::from(OsString::from_vec(b"/home/zeenix/\xff.rs".to_vec()));
        let error = path_to_uri(&path).unwrap_err().to_string();
        assert!(error.contains("UTF-8"), "{error}");
    }

    // The Windows tests below are what these conversions exist for: there a path and a URI have
    // next to nothing in common. Only the `windows-latest` CI job runs them.
    #[cfg(windows)]
    #[test]
    fn windows_paths_round_trip() {
        for path in [
            r"C:\Users\zeenix\src\lib.rs",
            r"C:\Users\zeenix\my crate\src\lib.rs",
        ] {
            let uri = path_to_uri(Path::new(path)).unwrap();
            assert!(uri.starts_with("file:///C:/"), "{path} became {uri}");
            assert!(!uri.contains('\\'), "{path} became {uri}");
            assert_eq!(normalize(&uri), uri, "{uri}");
            assert_eq!(uri_to_path(&uri).unwrap(), Path::new(path), "{uri}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_shares_round_trip() {
        let path = r"\\server\share\src\lib.rs";
        let uri = path_to_uri(Path::new(path)).unwrap();
        assert_eq!(uri, "file://server/share/src/lib.rs");
        assert_eq!(uri_to_path(&uri).unwrap(), Path::new(path));
    }

    #[cfg(windows)]
    #[test]
    fn absolute_keeps_the_plain_spelling_of_a_path() {
        // The extended-length form `canonicalize()` returns takes every path apart from there
        // on: `\\?\C:\dir` joined with `src/lib.rs` keeps that forward slash, and nothing on
        // Windows parses the result back apart.
        let absolute = absolute(&std::env::temp_dir());

        let spelling = absolute.to_str().unwrap();
        assert!(!spelling.starts_with(r"\\?\"), "{spelling}");
        assert!(absolute.join("a/b").parent().unwrap().ends_with("a"));
    }
}

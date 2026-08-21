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
    Url::from_file_path(path)
        .map(|url| url.to_string())
        .map_err(|()| anyhow!("Cannot make a file URI out of {}", path.display()))
}

/// The path `uri` names, or `None` if it is not a `file:` URI naming one.
///
/// Clients do pass URIs where a path is expected, so every path taken from a tool call goes
/// through here first.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let url = Url::parse(uri).ok()?;
    if url.scheme() != "file" {
        return None;
    }
    url.to_file_path().ok()
}

/// `uri` in the spelling used to key documents by.
///
/// The URIs we build and the ones rust-analyzer sends back name the same files but need not be
/// spelled identically: rust-analyzer learns of most files from cargo rather than from us, and on
/// Windows that means the drive letter's case is whatever cargo happened to print. Everything
/// else -- the percent-encoding above all -- both sides get from the same `url` crate.
pub fn normalize(uri: &str) -> String {
    let Ok(url) = Url::parse(uri) else {
        return uri.to_string();
    };
    if url.scheme() != "file" {
        return uri.to_string();
    }

    let Some(drive) = drive_letter(url.path()) else {
        return url.to_string();
    };

    let mut url = url;
    let normalized = format!("/{}{}", drive.to_ascii_uppercase(), &url.path()[2..]);
    // Only ever replaces one ASCII letter with another, so the URI stays valid.
    url.set_path(&normalized);
    url.to_string()
}

/// The drive letter of a `/C:/...`-shaped URI path.
fn drive_letter(path: &str) -> Option<char> {
    let mut chars = path.chars();
    let letter = match (chars.next(), chars.next(), chars.next()) {
        (Some('/'), Some(letter), Some(':')) if letter.is_ascii_alphabetic() => letter,
        _ => return None,
    };
    // The drive is a whole path segment: `/C:/x` is a drive, `/C:x` is not a path we understand.
    match chars.next() {
        Some('/') | None => Some(letter),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_upper_cases_the_drive_letter() {
        assert_eq!(
            normalize("file:///c:/Users/zeenix/src/lib.rs"),
            "file:///C:/Users/zeenix/src/lib.rs"
        );
        assert_eq!(
            normalize("file:///C:/Users/zeenix/src/lib.rs"),
            "file:///C:/Users/zeenix/src/lib.rs"
        );
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
        for uri in ["file:///c:/a.rs", "file:///home/zeenix/a.rs", "nonsense"] {
            assert_eq!(normalize(&normalize(uri)), normalize(uri), "{uri}");
        }
    }

    #[test]
    fn relative_paths_have_no_uri() {
        assert!(path_to_uri(Path::new("src/lib.rs")).is_err());
    }

    #[test]
    fn non_file_uris_have_no_path() {
        assert!(uri_to_path("http://example.com/lib.rs").is_none());
        assert!(uri_to_path("not a uri").is_none());
        assert!(uri_to_path("src/lib.rs").is_none());
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

    // Not exercised by CI, which is Linux-only, but this is the platform the conversions exist
    // for: on Windows a path and a URI have next to nothing in common.
    #[cfg(windows)]
    #[test]
    fn windows_paths_round_trip() {
        for path in [
            r"C:\Users\zeenix\src\lib.rs",
            r"C:\Users\zeenix\my crate\src\lib.rs",
            // The extended-length form `canonicalize()` returns.
            r"\\?\C:\Users\zeenix\src\lib.rs",
        ] {
            let uri = path_to_uri(Path::new(path)).unwrap();
            assert!(uri.starts_with("file:///C:/"), "{path} became {uri}");
            assert!(!uri.contains('\\'), "{path} became {uri}");
            assert_eq!(normalize(&uri), uri, "{uri}");
            // The round trip drops the extended-length prefix, naming the same file.
            let back = uri_to_path(&uri).unwrap();
            assert_eq!(back, Path::new(path.trim_start_matches(r"\\?\")), "{uri}");
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
}

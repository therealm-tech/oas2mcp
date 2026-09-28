//! Cursor pagination of `tools/list`. A cursor names an offset into one exact
//! list: the fingerprint of the list it was cut from travels with it, so a
//! cursor from a list that has since changed is refused rather than silently
//! skipping or repeating tools.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::num::NonZeroUsize;

/// Why a client-supplied cursor cannot be honoured.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CursorError {
    #[error("invalid cursor")]
    Malformed,
    #[error("stale cursor: the tool list changed since it was issued; list again from the start")]
    Stale,
}

/// One page of `items`, starting where `cursor` points, and the cursor of the
/// next page when there is one.
///
/// `fingerprint` identifies the exact list. Without a `page_size` the rest of
/// the list is one page, but a cursor is still honoured: a client may carry it
/// over from a replica that paginates.
pub fn page<'a, T>(
    items: &'a [T],
    fingerprint: u64,
    page_size: Option<NonZeroUsize>,
    cursor: Option<&str>,
) -> Result<(&'a [T], Option<String>), CursorError> {
    let start = match cursor {
        None => 0,
        Some(cursor) => {
            let (issued_for, offset) = decode(cursor).ok_or(CursorError::Malformed)?;
            if issued_for != fingerprint {
                return Err(CursorError::Stale);
            }
            // Only ever issued pointing at a non-empty remainder.
            if offset == 0 || offset >= items.len() {
                return Err(CursorError::Malformed);
            }
            offset
        }
    };
    let end = page_size.map_or(items.len(), |size| {
        start.saturating_add(size.get()).min(items.len())
    });
    let next = (end < items.len()).then(|| encode(fingerprint, end));
    Ok((&items[start..end], next))
}

/// Mix `parts` into one fingerprint.
///
/// `DefaultHasher::new` uses fixed keys, so every process of one build agrees on
/// a fingerprint: behind several stateless replicas serving the same document, a
/// cursor issued by one is honoured by the others.
pub fn fingerprint(parts: impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    parts.hash(&mut hasher);
    hasher.finish()
}

fn encode(fingerprint: u64, offset: usize) -> String {
    format!("{fingerprint:016x}.{offset:x}")
}

fn decode(cursor: &str) -> Option<(u64, usize)> {
    let (fingerprint, offset) = cursor.split_once('.')?;
    if fingerprint.len() != 16 {
        return None;
    }
    Some((
        u64::from_str_radix(fingerprint, 16).ok()?,
        usize::from_str_radix(offset, 16).ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(n: usize) -> Option<NonZeroUsize> {
        NonZeroUsize::new(n)
    }

    #[test]
    fn pages_concatenate_to_the_whole_list_and_the_last_has_no_cursor() {
        let items: Vec<u32> = (0..7).collect();
        let mut seen = Vec::new();
        let mut cursor = None;
        let mut pages = 0;
        loop {
            let (chunk, next) = page(&items, 42, size(3), cursor.as_deref()).expect("valid");
            assert!(chunk.len() <= 3);
            seen.extend_from_slice(chunk);
            pages += 1;
            match next {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        assert_eq!(seen, items);
        assert_eq!(pages, 3);
    }

    #[test]
    fn a_list_that_fits_one_page_carries_no_cursor() {
        let items = [1, 2, 3];
        for page_size in [None, size(3), size(10)] {
            let (chunk, next) = page(&items, 1, page_size, None).expect("valid");
            assert_eq!(chunk, items);
            assert_eq!(next, None);
        }
        let (chunk, next) = page::<u8>(&[], 1, size(2), None).expect("valid");
        assert!(chunk.is_empty());
        assert_eq!(next, None);
    }

    #[test]
    fn without_a_page_size_a_cursor_yields_the_rest() {
        let items = [1, 2, 3, 4];
        let (_, next) = page(&items, 9, size(1), None).expect("valid");
        let (rest, next) = page(&items, 9, None, next.as_deref()).expect("valid");
        assert_eq!(rest, [2, 3, 4]);
        assert_eq!(next, None);
    }

    #[test]
    fn a_garbage_cursor_is_malformed() {
        let items = [1, 2, 3];
        for cursor in [
            "",
            "garbage",
            "zz.1",
            "000000000000002a",
            "2a.1",
            "000000000000002a.",
            "000000000000002a.zz",
            // Offsets we never issue: the start, and past the end.
            "000000000000002a.0",
            "000000000000002a.3",
            "000000000000002a.ffffffffffffffffff",
        ] {
            assert_eq!(
                page(&items, 42, size(1), Some(cursor)),
                Err(CursorError::Malformed),
                "{cursor:?}"
            );
        }
    }

    #[test]
    fn a_cursor_from_another_list_is_stale() {
        let items = [1, 2, 3];
        let (_, next) = page(&items, 1, size(1), None).expect("valid");
        assert_eq!(
            page(&items, 2, size(1), next.as_deref()),
            Err(CursorError::Stale)
        );
    }

    #[test]
    fn the_fingerprint_follows_the_content() {
        assert_eq!(fingerprint(("a", 1)), fingerprint(("a", 1)));
        assert_ne!(fingerprint(("a", 1)), fingerprint(("a", 2)));
    }
}

//! Mapping source offsets across document revisions.

use exposition_core::document::TextPatch;

use crate::map::Affinity;

/// Map a UTF-16 `offset` through one revision's sorted, non-overlapping
/// `patches` (relative to the previous revision). Offsets inside a replaced
/// range move to its end (downstream) or start (upstream); insertions exactly
/// at the offset move it when the affinity is downstream.
pub fn map_offset(offset: u32, patches: &[TextPatch], affinity: Affinity) -> u32 {
    let mut shift: i64 = 0;
    for patch in patches {
        let start = patch.range.start;
        let end = patch.range.end;
        let inserted = patch.text.encode_utf16().count() as i64;
        if end < offset || (end == offset && start < end) {
            shift += inserted - i64::from(end - start);
        } else if start == offset && start == end {
            if affinity == Affinity::Downstream {
                shift += inserted;
            }
        } else if start < offset && offset < end {
            let mapped = match affinity {
                Affinity::Upstream => i64::from(start),
                Affinity::Downstream => i64::from(start) + inserted,
            };
            return (mapped + shift).max(0) as u32;
        } else if start >= offset {
            break;
        }
    }
    (i64::from(offset) + shift).max(0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_through_insertions_and_deletions() {
        let p = [TextPatch::insert(2, "xy"), TextPatch::delete(5, 7)];
        assert_eq!(map_offset(1, &p, Affinity::Downstream), 1);
        assert_eq!(map_offset(2, &p, Affinity::Downstream), 4);
        assert_eq!(map_offset(2, &p, Affinity::Upstream), 2);
        assert_eq!(map_offset(4, &p, Affinity::Downstream), 6);
        assert_eq!(map_offset(6, &p, Affinity::Upstream), 7);
        assert_eq!(map_offset(7, &p, Affinity::Downstream), 7);
        assert_eq!(map_offset(9, &p, Affinity::Downstream), 9);
    }
}

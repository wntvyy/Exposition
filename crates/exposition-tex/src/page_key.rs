//! Content identity of the pages of a PDF.
//!
//! A page's key is a hash of everything in the file its rendering can depend
//! on: the page object and every object reachable from it (content streams,
//! resources with their fonts, images and forms, annotations), the geometry
//! and resources it inherits from the page tree, and the document-level
//! output intents. Object numbers are not part of it, so two builds that
//! write the same page give it the same key, and a key names the same pixels
//! at a given scale for as long as the renderer is the same (keys are used
//! within one run of the application).
//!
//! Some pages get no key, and the caller then uses an identity that is unique
//! to the build:
//!
//! * anything this module cannot follow: a reference that does not resolve,
//!   an entry that does not parse, a walk that exceeds its bound;
//! * optional content. The renderer tells layers apart by object, not by
//!   content, so two layers with equal dictionaries are different to it and
//!   equal to a content hash: a document whose catalog has `OCProperties`
//!   has no keys at all, and neither has a page that reaches an optional
//!   content group, membership dictionary or `OC` entry.
//!
//! The page tree is never walked: the other pages, the page's ancestors and
//! the catalog hash as a marker when something refers to them (links,
//! annotations' `/P`, outlines). They are recognized by being those objects,
//! never by a `/Type` entry, which any dictionary may carry.

use std::collections::{HashMap, HashSet};

use hayro::hayro_syntax::Pdf;
use hayro::hayro_syntax::object::{Array, Dict, MaybeRef, Name, ObjRef, Object};
use hayro::hayro_syntax::page::Rotation;
use hayro::hayro_syntax::xref::XRef;
use sha2::{Digest, Sha256};

/// Changes whenever the hashed form below changes.
const VERSION: &[u8] = b"exposition-tex-page-key-2";

/// Indirect objects visited per file before giving up, at most.
const MAX_VISITS: usize = 2_000_000;

/// The visits a file of `objects` objects may take. An object whose content
/// is self-contained is hashed once; only objects on a cycle through one of
/// their ancestors are hashed again per path, and a graph that is dense in
/// such cycles has exponentially many paths. Giving up after a small multiple
/// of the object count keeps that case cheap (its pages get no key).
fn visit_bound(objects: usize) -> usize {
    objects.saturating_mul(16).saturating_add(4096).min(MAX_VISITS)
}

type Hash = [u8; 32];

/// "No reference back to an object that is still being hashed."
const CLOSED: usize = usize::MAX;

struct Walker<'a> {
    xref: &'a XRef,
    /// Digests of indirect objects whose content is self-contained: every
    /// cycle through them closes below them.
    done: HashMap<ObjRef, Hash>,
    /// Indirect objects being hashed, outermost first.
    open: Vec<ObjRef>,
    /// The pages, their ancestors in the page tree, and the catalog.
    page_tree: HashSet<ObjRef>,
    visits: usize,
    bound: usize,
}

fn bytes(out: &mut Sha256, tag: u8, data: &[u8]) {
    out.update([tag]);
    out.update((data.len() as u64).to_le_bytes());
    out.update(data);
}

/// Whether a dictionary is, or uses, optional content (see the module docs).
fn optional_content(dict: &Dict<'_>) -> bool {
    dict.contains_key("OC")
        || dict.contains_key("OCGs")
        || dict.get::<Name<'_>>("Type").is_some_and(|name| matches!(name.as_ref(), b"OCG" | b"OCMD"))
}

impl<'a> Walker<'a> {
    /// Hash a value. `Some(index)` is the outermost object in `open` that the
    /// value refers back to ([`CLOSED`] if none); `None` means it could not
    /// be followed.
    fn item(&mut self, item: MaybeRef<Object<'a>>, out: &mut Sha256) -> Option<usize> {
        match item {
            MaybeRef::NotRef(object) => self.object(&object, out),
            MaybeRef::Ref(reference) => self.reference(reference, out),
        }
    }

    fn reference(&mut self, reference: ObjRef, out: &mut Sha256) -> Option<usize> {
        if let Some(digest) = self.done.get(&reference) {
            bytes(out, 20, digest);
            return Some(CLOSED);
        }
        if let Some(position) = self.open.iter().position(|open| *open == reference) {
            // A cycle: name the target by its distance from here.
            out.update([21]);
            out.update(((self.open.len() - position) as u64).to_le_bytes());
            return Some(position);
        }
        // A page's rendering never depends on another page, on the page
        // tree or on the catalog (links and outlines point at pages).
        if self.page_tree.contains(&reference) {
            out.update([22]);
            return Some(CLOSED);
        }
        let object = self.xref.get::<Object<'a>>(reference.into())?;
        self.visits += 1;
        if self.visits > self.bound {
            return None;
        }
        let index = self.open.len();
        self.open.push(reference);
        let mut inner = Sha256::new();
        let lowest = self.object(&object, &mut inner);
        self.open.pop();
        let lowest = lowest?;
        let digest: Hash = inner.finalize().into();
        bytes(out, 20, &digest);
        if lowest >= index {
            // Nothing in it refers above it: the digest is the same wherever
            // the object is reached from, so it can be reused.
            self.done.insert(reference, digest);
            Some(CLOSED)
        } else {
            Some(lowest)
        }
    }

    fn object(&mut self, object: &Object<'a>, out: &mut Sha256) -> Option<usize> {
        match object {
            Object::Null(_) => out.update([0]),
            Object::Boolean(value) => out.update([1, u8::from(*value)]),
            Object::Number(value) => {
                out.update([2]);
                out.update(value.as_f64().to_bits().to_le_bytes());
            }
            Object::String(value) => bytes(out, 3, value.as_ref()),
            Object::Name(value) => bytes(out, 4, value.as_ref()),
            Object::Dict(dict) => {
                out.update([5]);
                return self.dict(dict, &[], out);
            }
            Object::Array(array) => {
                out.update([6]);
                return self.array(array, out);
            }
            Object::Stream(stream) => {
                out.update([7]);
                let lowest = self.dict(stream.dict(), &[], out)?;
                bytes(out, 8, &stream.raw_data());
                return Some(lowest);
            }
        }
        Some(CLOSED)
    }

    /// Hash a dictionary's entries (in key order) except the keys in `skip`.
    fn dict(&mut self, dict: &Dict<'a>, skip: &[&[u8]], out: &mut Sha256) -> Option<usize> {
        if optional_content(dict) {
            return None;
        }
        let entries: Vec<_> = dict.entries().collect();
        // An entry that does not parse is left out by `entries`.
        if entries.len() != dict.len() {
            return None;
        }
        let mut lowest = CLOSED;
        for (key, value) in entries {
            if skip.contains(&key.as_ref()) {
                continue;
            }
            bytes(out, 9, key.as_ref());
            lowest = lowest.min(self.item(value, out)?);
        }
        out.update([10]);
        Some(lowest)
    }

    fn array(&mut self, array: &Array<'a>, out: &mut Sha256) -> Option<usize> {
        let mut lowest = CLOSED;
        for item in array.raw_iter() {
            lowest = lowest.min(self.item(item, out)?);
        }
        out.update([11]);
        Some(lowest)
    }
}

/// The content key of every page (40 hexadecimal characters), `None` for a
/// page whose content could not be followed completely.
pub fn page_keys(pdf: &Pdf) -> Vec<Option<String>> {
    page_keys_counted(pdf).0
}

/// [`page_keys`] and the number of indirect objects the walk visited (at most
/// the file's visit bound plus one).
pub fn page_keys_counted(pdf: &Pdf) -> (Vec<Option<String>>, usize) {
    let xref = pdf.xref();
    let pages = pdf.pages();
    // The page tree as objects: every page, its ancestors, the catalog.
    let mut page_tree: HashSet<ObjRef> = HashSet::new();
    page_tree.insert(xref.root_id().into());
    for page in pages.iter() {
        if let Some(id) = page.raw().obj_id() {
            page_tree.insert(id.into());
        }
        let mut parent = page.raw().get_ref("Parent");
        while let Some(reference) = parent {
            if !page_tree.insert(reference) {
                break; // reached from another page already (or a loop)
            }
            parent = xref.get::<Dict<'_>>(reference.into()).and_then(|node| node.get_ref("Parent"));
        }
    }
    let mut walker =
        Walker { xref, done: HashMap::new(), open: Vec::new(), page_tree, visits: 0, bound: visit_bound(pdf.len()) };
    // Document-level settings a page's rendering can depend on.
    let mut salt = Sha256::new();
    salt.update(VERSION);
    let document = xref.get::<Dict<'_>>(xref.root_id()).and_then(|catalog| {
        // Layers are identified by object in the renderer: no content key.
        if catalog.contains_key("OCProperties") {
            return None;
        }
        let key = &b"OutputIntents"[..];
        match catalog.get_raw::<Object<'_>>(key) {
            Some(value) => {
                bytes(&mut salt, 30, key);
                walker.item(value, &mut salt)?;
            }
            None if catalog.contains_key(key) => return None,
            None => {}
        }
        Some(())
    });
    if document.is_none() {
        return (vec![None; pages.len()], walker.visits);
    }
    let salt: Hash = salt.finalize().into();
    let keys = pages
        .iter()
        .map(|page| {
            let mut out = Sha256::new();
            out.update(salt);
            // Geometry and resources as the renderer resolves them (they can
            // be inherited from the page tree, which is not walked).
            for rect in [page.media_box(), page.crop_box()] {
                for value in [rect.x0, rect.y0, rect.x1, rect.y1] {
                    out.update(value.to_bits().to_le_bytes());
                }
            }
            out.update([match page.rotation() {
                Rotation::None => 0u8,
                Rotation::Horizontal => 1,
                Rotation::Flipped => 2,
                Rotation::FlippedHorizontal => 3,
            }]);
            let resources = page.resources();
            for dict in [
                &resources.ext_g_states,
                &resources.fonts,
                &resources.properties,
                &resources.color_spaces,
                &resources.x_objects,
                &resources.patterns,
                &resources.shadings,
            ] {
                walker.dict(dict, &[], &mut out)?;
            }
            walker.dict(page.raw(), &[b"Parent"], &mut out)?;
            let digest: Hash = out.finalize().into();
            Some(digest[..20].iter().map(|b| format!("{b:02x}")).collect())
        })
        .collect();
    (keys, walker.visits)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// A PDF file with the given indirect objects (number, body).
    fn pdf(objects: &[(u32, String)]) -> Pdf {
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = BTreeMap::new();
        for (number, body) in objects {
            offsets.insert(*number, out.len());
            out.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        let max = *offsets.keys().max().unwrap();
        let xref_at = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", max + 1).as_bytes());
        for number in 1..=max {
            match offsets.get(&number) {
                Some(offset) => out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes()),
                None => out.extend_from_slice(b"0000000000 65535 f \n"),
            }
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n", max + 1).as_bytes(),
        );
        Pdf::new(out).expect("a loadable PDF")
    }

    fn stream(extra: &str, data: &str) -> String {
        format!("<< /Length {} {extra} >>\nstream\n{data}\nendstream", data.len())
    }

    /// Two pages sharing a font; the second also draws a form. `shift` moves
    /// every object number except the catalog's.
    struct Parts {
        shift: u32,
        media_box: &'static str,
        first: &'static str,
        second: &'static str,
        font: &'static str,
        form: &'static str,
        extra_second: String,
    }

    impl Default for Parts {
        fn default() -> Self {
            Parts {
                shift: 0,
                media_box: "[0 0 300 400]",
                first: "BT /F1 12 Tf 20 300 Td (one) Tj ET",
                second: "BT /F1 12 Tf 20 300 Td (two) Tj ET /Fm1 Do",
                font: "/Helvetica",
                form: "0 0 10 10 re f",
                extra_second: String::new(),
            }
        }
    }

    fn document(parts: &Parts) -> Vec<Option<String>> {
        let n = |number: u32| number + parts.shift;
        let objects = vec![
            (1, format!("<< /Type /Catalog /Pages {} 0 R >>", n(2))),
            (
                n(2),
                format!("<< /Type /Pages /Kids [{} 0 R {} 0 R] /Count 2 /MediaBox {} >>", n(3), n(4), parts.media_box),
            ),
            (
                n(3),
                format!(
                    "<< /Type /Page /Parent {} 0 R /Contents {} 0 R /Resources << /Font << /F1 {} 0 R >> >> >>",
                    n(2),
                    n(5),
                    n(7)
                ),
            ),
            (
                n(4),
                format!(
                    "<< /Type /Page /Parent {} 0 R /Contents {} 0 R /Resources << /Font << /F1 {} 0 R >> /XObject << /Fm1 {} 0 R >> >> {} >>",
                    n(2),
                    n(6),
                    n(7),
                    n(8),
                    parts.extra_second
                ),
            ),
            (n(5), stream("", parts.first)),
            (n(6), stream("", parts.second)),
            (n(7), format!("<< /Type /Font /Subtype /Type1 /BaseFont {} >>", parts.font)),
            (n(8), stream("/Type /XObject /Subtype /Form /BBox [0 0 10 10]", parts.form)),
        ];
        page_keys(&pdf(&objects))
    }

    #[test]
    fn pages_have_distinct_keys_that_do_not_depend_on_object_numbers() {
        let keys = document(&Parts::default());
        assert_eq!(keys.len(), 2);
        let (first, second) = (keys[0].clone().expect("key"), keys[1].clone().expect("key"));
        assert_eq!(first.len(), 40);
        assert_ne!(first, second);
        // The same pages written with other object numbers (another build).
        assert_eq!(document(&Parts { shift: 20, ..Parts::default() }), keys);
    }

    #[test]
    fn a_key_changes_exactly_when_something_the_page_draws_changes() {
        let base = document(&Parts::default());
        // Another page's text.
        let edited = document(&Parts { second: "BT /F1 12 Tf 20 300 Td (2) Tj ET /Fm1 Do", ..Parts::default() });
        assert_eq!(edited[0], base[0]);
        assert_ne!(edited[1], base[1]);
        // A resource only the second page uses.
        let form = document(&Parts { form: "0 0 5 5 re f", ..Parts::default() });
        assert_eq!(form[0], base[0]);
        assert_ne!(form[1], base[1]);
        // A font both pages use.
        let font = document(&Parts { font: "/Times-Roman", ..Parts::default() });
        assert_ne!(font[0], base[0]);
        assert_ne!(font[1], base[1]);
        // A page size inherited from the page tree.
        let size = document(&Parts { media_box: "[0 0 300 500]", ..Parts::default() });
        assert_ne!(size[0], base[0]);
        assert_ne!(size[1], base[1]);
        // An annotation's appearance (drawn with the page).
        let annotated = document(&Parts {
            extra_second: "/Annots [<< /Type /Annot /Subtype /Square /Rect [1 1 9 9] /Border [0 0 2] >>]".into(),
            ..Parts::default()
        });
        assert_eq!(annotated[0], base[0]);
        assert_ne!(annotated[1], base[1]);
    }

    #[test]
    fn references_back_to_pages_and_cycles_are_followed_once() {
        // A link annotation pointing at its own page and at the other page.
        let linked = Parts {
            extra_second: "/Annots [<< /Type /Annot /Subtype /Link /Rect [1 1 9 9] /P 4 0 R /Dest [3 0 R /Fit] >>]"
                .into(),
            ..Parts::default()
        };
        let keys = document(&linked);
        assert!(keys.iter().all(Option::is_some), "{keys:?}");
        assert_eq!(document(&linked), keys);
        // The link target's text changes: the linking page is drawn the same.
        let retargeted = document(&Parts { first: "BT /F1 12 Tf 20 300 Td (1) Tj ET", ..linked });
        assert_eq!(retargeted[1], keys[1]);
        assert_ne!(retargeted[0], keys[0]);
        // Two objects that refer to each other.
        let cyclic = |text: &str| {
            page_keys(&pdf(&[
                (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 100 100] >>".into()),
                (
                    3,
                    "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /ExtGState << /G 5 0 R >> >> >>".into(),
                ),
                (4, stream("", "/G gs")),
                (5, "<< /Type /ExtGState /Peer 6 0 R >>".into()),
                (6, format!("<< /Back 5 0 R /Note ({text}) >>")),
            ]))
        };
        let keys = cyclic("a");
        assert!(keys[0].is_some());
        assert_eq!(cyclic("a"), keys);
        assert_ne!(cyclic("b"), keys, "content inside a cycle still counts");
    }

    #[test]
    fn a_page_that_cannot_be_followed_has_no_key() {
        let keys = page_keys(&pdf(&[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 100 100] >>".into()),
            (3, "<< /Type /Page /Parent 2 0 R /Contents 5 0 R /Resources << /Font << /F1 9 0 R >> >> >>".into()),
            (4, "<< /Type /Page /Parent 2 0 R /Contents 5 0 R >>".into()),
            (5, stream("", "0 0 1 1 re f")),
        ]));
        assert_eq!(keys.len(), 2);
        assert!(keys[0].is_none(), "the font reference does not resolve");
        assert!(keys[1].is_some());
    }
}

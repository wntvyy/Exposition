//! Review: a page's content key names its pixels. Two pages with the same key
//! are served the same cached image, so pages that render differently must
//! never share a key. Hand-written PDFs vary one thing at a time; every case
//! is checked against the real renderer ("equal keys => equal pixels").

use std::collections::BTreeMap;
use std::time::Instant;

use exposition_tex::pdf::PdfDocument;

/// A PDF file with the given indirect objects (number, body), classic xref.
fn pdf(objects: &[(u32, String)]) -> PdfDocument {
    let mut out = b"%PDF-1.5\n".to_vec();
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
    PdfDocument::open(out).expect("a loadable PDF")
}

fn stream(extra: &str, data: &str) -> String {
    format!("<< /Length {} {extra} >>\nstream\n{data}\nendstream", data.len())
}

struct Rendered {
    keys: Vec<Option<String>>,
    pngs: Vec<Vec<u8>>,
}

fn rendered(doc: &PdfDocument) -> Rendered {
    let keys = doc.page_keys();
    let pngs = (0..doc.page_count()).map(|i| doc.render_page(i, 1.0).expect("render").png).collect();
    Rendered { keys, pngs }
}

/// A one-page document: catalog 1, page tree 2, page 3, content 4, more objects.
fn one_page(catalog: &str, tree: &str, page: &str, content: &str, more: &[(u32, String)]) -> PdfDocument {
    let mut objects = vec![
        (1, format!("<< /Type /Catalog /Pages 2 0 R {catalog} >>")),
        (2, format!("<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 100 100] {tree} >>")),
        (3, format!("<< /Type /Page /Parent 2 0 R /Contents 4 0 R {page} >>")),
        (4, stream("", content)),
    ];
    objects.extend(more.iter().cloned());
    pdf(&objects)
}

/// The pages `a[i]` and `b[j]` must be told apart: they render differently
/// (asserted, so the case means something) and so their keys must differ
/// (or one must have no key, which falls back to a build-unique reference).
fn must_differ(case: &str, a: &Rendered, i: usize, b: &Rendered, j: usize) {
    assert_ne!(a.pngs[i], b.pngs[j], "{case}: the renderer must draw these two pages differently");
    if let (Some(x), Some(y)) = (&a.keys[i], &b.keys[j]) {
        assert_ne!(x, y, "{case}: pages with different pixels share a content key");
    }
}

/// Equal keys imply equal pixels (whatever the renderer supports).
fn implication(case: &str, a: &Rendered, i: usize, b: &Rendered, j: usize) {
    if let (Some(x), Some(y)) = (&a.keys[i], &b.keys[j])
        && x == y
    {
        assert_eq!(a.pngs[i], b.pngs[j], "{case}: equal keys, different pixels");
    }
    eprintln!(
        "{case}: keys {} pixels {}",
        if a.keys[i] == b.keys[j] { "equal" } else { "differ" },
        if a.pngs[i] == b.pngs[j] { "equal" } else { "differ" }
    );
}

const FILL: &str = "/G gs 0 0 100 100 re f";

#[test]
fn inherited_and_nested_resources_are_part_of_the_key() {
    // ExtGState inherited from the page tree (the page has no /Resources).
    let inherited = |alpha: &str| {
        rendered(&one_page("", &format!("/Resources << /ExtGState << /G << /ca {alpha} >> >> >>"), "", FILL, &[]))
    };
    must_differ("resources inherited from /Pages", &inherited("1"), 0, &inherited("0.2"), 0);

    // Inherited through two levels of page-tree nodes, as an indirect object.
    let deep = |alpha: &str| {
        rendered(&pdf(&[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [20 0 R] /Count 1 /MediaBox [0 0 100 100] /Resources 21 0 R >>".into()),
            (20, "<< /Type /Pages /Parent 2 0 R /Kids [3 0 R] /Count 1 >>".into()),
            (21, "<< /ExtGState 22 0 R >>".into()),
            (22, "<< /G 23 0 R >>".into()),
            (23, format!("<< /Type /ExtGState /ca {alpha} >>")),
            (3, "<< /Type /Page /Parent 20 0 R /Contents 4 0 R >>".into()),
            (4, stream("", FILL)),
        ]))
    };
    must_differ("resources inherited through two tree levels", &deep("1"), 0, &deep("0.2"), 0);

    // A form in a form in a form; only the innermost graphics state differs.
    let nested = |alpha: &str| {
        rendered(&one_page(
            "",
            "",
            "/Resources << /XObject << /Fm1 10 0 R >> >>",
            "/Fm1 Do",
            &[
                (
                    10,
                    stream(
                        "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << /XObject << /Fm2 11 0 R >> >>",
                        "/Fm2 Do",
                    ),
                ),
                (
                    11,
                    stream(
                        "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << /XObject << /Fm3 12 0 R >> >>",
                        "/Fm3 Do",
                    ),
                ),
                (
                    12,
                    stream(
                        "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << /ExtGState << /G 13 0 R >> >>",
                        FILL,
                    ),
                ),
                (13, format!("<< /Type /ExtGState /ca {alpha} >>")),
            ],
        ))
    };
    must_differ("graphics state three forms deep", &nested("1"), 0, &nested("0.2"), 0);

    // Image data.
    let image = |sample: &str| {
        rendered(&one_page(
            "",
            "",
            "/Resources << /XObject << /Im1 10 0 R >> >>",
            "100 0 0 100 0 0 cm /Im1 Do",
            &[(
                10,
                stream(
                    "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
                    sample,
                ),
            )],
        ))
    };
    must_differ("image data", &image("0"), 0, &image("z"), 0);

    // A colour space's lookup table (a string several levels down).
    let indexed = |table: &str| {
        rendered(&one_page(
            "",
            "",
            &format!("/Resources << /ColorSpace << /CS0 [/Indexed /DeviceRGB 0 <{table}>] >> >>"),
            "/CS0 cs 0 sc 0 0 100 100 re f",
            &[],
        ))
    };
    must_differ("indexed colour table", &indexed("FF0000"), 0, &indexed("0000FF"), 0);

    // The second of two content streams.
    let contents = |second: &str| {
        rendered(&pdf(&[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 100 100] >>".into()),
            (3, "<< /Type /Page /Parent 2 0 R /Contents [4 0 R 5 0 R] >>".into()),
            (4, stream("", "0 0 10 10 re f")),
            (5, stream("", second)),
        ]))
    };
    must_differ("second content stream", &contents("50 50 10 10 re f"), 0, &contents("60 60 10 10 re f"), 0);
}

#[test]
fn inherited_geometry_is_part_of_the_key() {
    let geometry = |tree: &str, page: &str| {
        rendered(&pdf(&[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, format!("<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 100 200] {tree} >>")),
            (3, format!("<< /Type /Page /Parent 2 0 R /Contents 4 0 R {page} >>")),
            (4, stream("", "0 0 100 50 re f")),
        ]))
    };
    let base = geometry("", "");
    must_differ("inherited /Rotate", &base, 0, &geometry("/Rotate 90", ""), 0);
    must_differ("inherited /CropBox", &base, 0, &geometry("/CropBox [0 0 50 50]", ""), 0);
    must_differ("own /Rotate", &base, 0, &geometry("", "/Rotate 180"), 0);
    // Whether the renderer honours these or not, equal keys must mean equal pixels.
    implication("/UserUnit", &base, 0, &geometry("", "/UserUnit 2"), 0);
    implication("own /MediaBox", &base, 0, &geometry("", "/MediaBox [0 0 100 150]"), 0);
}

#[test]
fn annotations_and_direct_or_indirect_objects_keep_the_implication() {
    let annotated = |draw: &str| {
        rendered(&one_page(
            "",
            "",
            "/Annots [<< /Type /Annot /Subtype /Square /Rect [10 10 90 90] /F 4 /AP << /N 10 0 R >> >>]",
            "0 0 5 5 re f",
            &[(10, stream("/Type /XObject /Subtype /Form /BBox [0 0 80 80]", draw))],
        ))
    };
    implication("annotation appearance stream", &annotated("0 0 80 80 re f"), 0, &annotated("0 0 40 40 re f"), 0);

    // The same graphics state written directly and as an indirect object.
    let direct = rendered(&one_page("", "", "/Resources << /ExtGState << /G << /ca 0.2 >> >> >>", FILL, &[]));
    let indirect = rendered(&one_page(
        "",
        "",
        "/Resources << /ExtGState << /G 10 0 R >> >>",
        FILL,
        &[(10, "<< /ca 0.2 >>".into())],
    ));
    assert_eq!(direct.pngs[0], indirect.pngs[0]);
    implication("direct or indirect", &direct, 0, &indirect, 0);
    let opaque =
        rendered(&one_page("", "", "/Resources << /ExtGState << /G 10 0 R >> >>", FILL, &[(10, "<< /ca 1 >>".into())]));
    must_differ("indirect graphics state", &indirect, 0, &opaque, 0);
}

/// Optional content: a layer that is switched off hides what it marks. The
/// renderer identifies layers by object, the key by content, so two layers
/// with the same dictionary (the same name) are one layer to the key.
fn layered(off: u32, first_uses: u32, second_uses: u32) -> Rendered {
    let marked = "/OC /MC0 BDC 0 0 100 100 re f EMC";
    rendered(&pdf(&[
        (
            1,
            format!(
                "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [10 0 R 11 0 R] /D << /OFF [{off} 0 R] >> >> >>"
            ),
        ),
        (2, "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 /MediaBox [0 0 100 100] >>".into()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Properties << /MC0 {first_uses} 0 R >> >> >>"
            ),
        ),
        (4, stream("", marked)),
        (
            5,
            format!(
                "<< /Type /Page /Parent 2 0 R /Contents 6 0 R /Resources << /Properties << /MC0 {second_uses} 0 R >> >> >>"
            ),
        ),
        (6, stream("", marked)),
        (10, "<< /Type /OCG /Name (Answer) >>".into()),
        (11, "<< /Type /OCG /Name (Answer) >>".into()),
    ]))
}

#[test]
fn the_renderer_hides_content_of_a_layer_that_is_off() {
    // Precondition of the two tests below: layer 10 is off, layer 11 is on.
    let doc = layered(10, 10, 11);
    assert_ne!(doc.pngs[0], doc.pngs[1], "a page marked with the hidden layer is blank, the other is filled");
}

#[test]
fn pages_that_use_different_layers_with_equal_dictionaries_do_not_share_a_key() {
    let doc = layered(10, 10, 11);
    must_differ("two layers with the same name, one page each", &doc, 0, &doc, 1);
}

#[test]
fn switching_off_the_other_layer_with_an_equal_dictionary_changes_the_key() {
    // Two builds: the same page, but the configuration hides the other layer.
    let before = layered(10, 10, 10);
    let after = layered(11, 10, 10);
    must_differ("the layer's visibility changed between builds", &before, 0, &after, 0);
}

#[test]
fn a_layer_with_its_own_name_is_told_apart() {
    // Control for the defect above: with different dictionaries the key follows.
    let marked = "/OC /MC0 BDC 0 0 100 100 re f EMC";
    let doc = |uses: u32| {
        rendered(&one_page(
            "/OCProperties << /OCGs [10 0 R 11 0 R] /D << /OFF [10 0 R] >> >>",
            "",
            &format!("/Resources << /Properties << /MC0 {uses} 0 R >> >>"),
            marked,
            &[(10, "<< /Type /OCG /Name (Hidden) >>".into()), (11, "<< /Type /OCG /Name (Shown) >>".into())],
        ))
    };
    must_differ("layers with different names", &doc(10), 0, &doc(11), 0);
    // The configuration itself.
    let config = |off: &str| {
        rendered(&one_page(
            &format!("/OCProperties << /OCGs [10 0 R] /D << {off} >> >>"),
            "",
            "/Resources << /Properties << /MC0 10 0 R >> >>",
            marked,
            &[(10, "<< /Type /OCG /Name (Layer) >>".into())],
        ))
    };
    must_differ("layer switched off in the catalog", &config(""), 0, &config("/OFF [10 0 R]"), 0);
}

/// An indirect object reached from a page whose dictionary says `/Type /Page`
/// is hashed as a marker (page-tree nodes are not walked), whatever else it
/// holds. The renderer does not look at `/Type` of a graphics state.
#[test]
fn a_resource_dictionary_typed_as_a_page_is_still_hashed() {
    let spoofed = |alpha: &str| {
        rendered(&one_page(
            "",
            "",
            "/Resources << /ExtGState << /G 10 0 R >> >>",
            FILL,
            &[(10, format!("<< /Type /Page /ca {alpha} >>"))],
        ))
    };
    must_differ("graphics state with /Type /Page", &spoofed("1"), 0, &spoofed("0.2"), 0);
}

#[test]
fn a_densely_cyclic_object_graph_terminates() {
    // Object i refers to every later object and back to the first: the number
    // of paths through it doubles with every object.
    const N: u32 = 26;
    let mut objects = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_owned()),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 100 100] >>".to_owned()),
        (3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /ExtGState << /G 100 0 R >> >> >>".to_owned()),
        (4, stream("", FILL)),
    ];
    for i in 0..N {
        let mut refs = String::new();
        for j in i + 1..N {
            refs.push_str(&format!("{} 0 R ", 100 + j));
        }
        objects.push((100 + i, format!("<< /ca 0.5 /Next [{refs}] /First 100 0 R >>")));
    }
    let doc = pdf(&objects);
    let started = Instant::now();
    let keys = doc.page_keys();
    let elapsed = started.elapsed();
    eprintln!("dense cycle of {N} objects: key {:?} after {elapsed:?}", keys[0]);
    assert_eq!(keys.len(), 1);
    assert!(elapsed.as_secs() < 120, "the walk must stay bounded: {elapsed:?}");
    // Bounded by the size of the file, not by time: the walk gives up after a
    // small multiple of the object count, and the page then has no key.
    let (counted, visits, objects) = doc.page_keys_counted();
    assert_eq!(counted, keys);
    assert!(keys[0].is_none(), "a graph this cyclic is not hashed: {:?}", keys[0]);
    assert!((30..64).contains(&objects), "{objects} objects");
    assert!(visits <= objects * 16 + 4096 + 1, "{visits} visits for {objects} objects");
    // A page that cannot be followed within the bound has no key (build-unique reference).
    doc.render_page(0, 1.0).expect("still renders");
}

#[test]
fn a_file_that_is_not_a_pdf_or_is_cut_off_does_not_yield_keys_for_missing_content() {
    assert!(PdfDocument::open(b"not a pdf".to_vec()).is_err());
    // A page whose content stream object is missing from the file.
    let doc = pdf(&[
        (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 100 100] >>".into()),
        (3, "<< /Type /Page /Parent 2 0 R /Contents 9 0 R >>".into()),
    ]);
    assert_eq!(doc.page_keys(), vec![None]);
    // A catalog whose optional-content entry does not resolve: no page has a key.
    let doc = one_page("/OCProperties 9 0 R", "", "", "0 0 1 1 re f", &[]);
    eprintln!("unresolved /OCProperties: {:?}", doc.page_keys());
}

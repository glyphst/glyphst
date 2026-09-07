use super::*;

fn snapshot(unit: f64, rotation: u16) -> DocumentSnapshot {
    let content = b"0 0 1 rg 20 30 80 40 re f\n";
    let objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        format!("<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [10 20 110 80] /CropBox [20 30 100 70] /Rotate {rotation} /UserUnit 9 >>").into_bytes(),
        format!("<< /Type /Page /Parent 2 0 R /UserUnit {unit} /Resources << >> /Contents 4 0 R /Annots [5 0 R] >>").into_bytes(),
        [format!("<< /Length {} >>\nstream\n",content.len()).as_bytes(),content,b"endstream"].concat(),
        b"<< /Type /Annot /Subtype /Link /Rect [25 35 45 45] /Border [0 0 0] /H /N /Dest [3 0 R /XYZ 30 60 1.25] >>".to_vec(),
    ];
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        bytes.extend_from_slice(object);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    DocumentSnapshot::from_pdf_bytes("physical-points.pdf", bytes)
}

#[test]
fn user_unit_page_sizes_and_link_regions_match_reader_mupdf_geometry() {
    for rotation in [0, 90, 180, 270] {
        for unit in [0.5, 2.0, 2.5] {
            let snapshot = snapshot(unit, rotation);
            let native = GpuPdfDocument::open(&snapshot).expect("native document");
            let compiled = native
                .compile_page(0, &CancellationToken::new())
                .expect("compile physical page");
            assert!(
                compiled.support().is_supported(),
                "{:?}",
                compiled.support()
            );
            let reader = snapshot.open().expect("reader document");
            let page = reader.inner().load_page(0).expect("reader page");
            let reader_bounds = page.bounds().expect("reader bounds");
            let bounds = compiled.scene().page_bounds();
            for (actual, expected) in [bounds.x0, bounds.y0, bounds.x1, bounds.y1]
                .into_iter()
                .zip([
                    reader_bounds.x0,
                    reader_bounds.y0,
                    reader_bounds.x1,
                    reader_bounds.y1,
                ])
            {
                assert!(
                    (actual - f64::from(expected)).abs() < 0.01,
                    "unit={unit} rotation={rotation}: {bounds:?} vs {reader_bounds:?}"
                );
            }
            let links = page.links().expect("reader links").collect::<Vec<_>>();
            let interactions = compiled_link_interactions(&compiled).expect("native interactions");
            assert_eq!(links.len(), 1);
            assert_eq!(interactions.len(), 1);
            let bounds = links[0].bounds;
            for (actual, expected) in interactions[0]
                .annotation_bounds
                .into_iter()
                .zip([bounds.x0, bounds.y0, bounds.x1, bounds.y1])
            {
                assert!(
                    (actual - f64::from(expected)).abs() < 0.01,
                    "unit={unit} rotation={rotation}: link geometry"
                );
            }
            let (x, y) = match rotation {
                0 => (10.0, 10.0),
                90 => (30.0, 10.0),
                180 => (70.0, 30.0),
                270 => (10.0, 70.0),
                _ => unreachable!(),
            };
            assert_eq!(
                interactions[0].action,
                GpuLinkAction::GoTo {
                    page: 0,
                    view: GpuDestinationView::Xyz {
                        x: Some(x * unit),
                        y: Some(y * unit),
                        zoom: Some(1.25)
                    }
                }
            );
            let again = native
                .compile_page(0, &CancellationToken::new())
                .expect("cached page");
            assert!(Arc::ptr_eq(&compiled, &again));
        }
    }
}

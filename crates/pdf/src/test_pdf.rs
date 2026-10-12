//! Small PDFs built in memory for tests.

/// A 200×100 pt page with `content`, page `resources`, extra page keys
/// (`page_extra`, e.g. `/Annots [5 0 R]`) and `extra` objects numbered from 5.
pub fn pdf_with(content: &str, resources: &str, page_extra: &str, extra: &[&str]) -> Vec<u8> {
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] \
             /Resources {resources} /Contents 4 0 R {page_extra} >>"
        ),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len() + 1
        ),
    ];
    objects.extend(extra.iter().map(|object| object.to_string()));
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

pub fn single_page_pdf(content: &str, resources: &str, page_extra: &str) -> Vec<u8> {
    pdf_with(content, resources, page_extra, &[])
}

/// Page resources with Helvetica as `/F1`.
pub const HELVETICA: &str =
    "<< /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >>";

/// A page with `lines` of 12 pt Helvetica from the top left.
pub fn text_pdf(lines: &[&str]) -> Vec<u8> {
    let mut content = String::from("BT /F1 12 Tf 10 80 Td");
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            content.push_str(" 0 -16 Td");
        }
        content.push_str(&format!(" ({line}) Tj"));
    }
    content.push_str(" ET");
    single_page_pdf(&content, HELVETICA, "")
}

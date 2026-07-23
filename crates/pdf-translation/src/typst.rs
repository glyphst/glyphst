use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use mupdf::{Colorspace, Document, Matrix};

use crate::{
    BoundingBox, CancellationToken, FontFamilyClass, LayoutDocument, LayoutItem, OverflowPolicy,
    StyledRun, TextStyle,
};

#[derive(Debug, Clone)]
pub struct TypstCompilerConfig {
    pub executable: OsString,
    pub assets_dir: PathBuf,
}

impl Default for TypstCompilerConfig {
    fn default() -> Self {
        Self {
            executable: std::env::var_os("GLYPHST_TYPST_BIN")
                .unwrap_or_else(|| OsString::from("typst")),
            assets_dir: resolve_translation_assets_dir(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TypstCompiler {
    config: TypstCompilerConfig,
}

impl TypstCompiler {
    pub fn new(config: TypstCompilerConfig) -> Self {
        Self { config }
    }

    pub fn compile(
        &self,
        layout: &LayoutDocument,
        job_directory: &Path,
        cancellation: &CancellationToken,
    ) -> Result<TypstOutput, TypstError> {
        self.compile_source(
            generate_typst_source(layout),
            "translation-overlay",
            job_directory,
            cancellation,
        )
    }

    pub fn compile_translated_pdf(
        &self,
        source_pdf: &Path,
        layout: &LayoutDocument,
        job_directory: &Path,
        cancellation: &CancellationToken,
    ) -> Result<TypstOutput, TypstError> {
        fs::create_dir_all(job_directory).map_err(|error| {
            TypstError::io(format!(
                "failed to create Typst job directory {}: {error}",
                job_directory.display()
            ))
        })?;
        let copied_source = job_directory.join("source.pdf");
        fs::copy(source_pdf, &copied_source).map_err(|error| {
            TypstError::io(format!(
                "failed to stage source PDF {}: {error}",
                source_pdf.display()
            ))
        })?;
        let colors = sample_layout_background_colors(source_pdf, layout)?;
        self.compile_source(
            generate_translated_pdf_source(layout, "source.pdf", &colors),
            "translated-document",
            job_directory,
            cancellation,
        )
    }

    fn compile_source(
        &self,
        source: String,
        file_stem: &str,
        job_directory: &Path,
        cancellation: &CancellationToken,
    ) -> Result<TypstOutput, TypstError> {
        cancellation.check().map_err(|_| TypstError::cancelled())?;
        fs::create_dir_all(job_directory).map_err(|error| {
            TypstError::io(format!(
                "failed to create Typst job directory {}: {error}",
                job_directory.display()
            ))
        })?;
        let source_path = job_directory.join(format!("{file_stem}.typ"));
        let output_path = job_directory.join(format!("{file_stem}.pdf"));
        fs::write(&source_path, source).map_err(|error| {
            TypstError::io(format!(
                "failed to write Typst source {}: {error}",
                source_path.display()
            ))
        })?;

        let font_path = self.config.assets_dir.join("fonts");
        let package_path = self.config.assets_dir.join("typst/packages");
        validate_asset(&font_path, "translation font directory")?;
        validate_asset(&package_path, "Typst package directory")?;
        let mut child = Command::new(&self.config.executable)
            .arg("compile")
            .arg("--root")
            .arg(job_directory)
            .arg("--font-path")
            .arg(&font_path)
            .arg("--ignore-system-fonts")
            .arg("--package-path")
            .arg(&package_path)
            .arg("--package-cache-path")
            .arg(&package_path)
            .arg(&source_path)
            .arg(&output_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                TypstError::process(format!(
                    "failed to start Typst executable {:?}: {error}",
                    self.config.executable
                ))
            })?;

        let status = loop {
            if cancellation.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(TypstError::cancelled());
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(Duration::from_millis(80)),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(TypstError::process(format!(
                        "failed while waiting for Typst: {error}"
                    )));
                }
            }
        };
        let stdout = read_pipe(child.stdout.take());
        let stderr = read_pipe(child.stderr.take());
        if !status.success() {
            let detail = if stderr.trim().is_empty() {
                stdout.trim()
            } else {
                stderr.trim()
            };
            return Err(TypstError::compile(if detail.is_empty() {
                "Typst compilation failed without diagnostics".to_string()
            } else {
                detail.chars().take(8_000).collect()
            }));
        }
        if !output_path.is_file() {
            return Err(TypstError::compile(
                "Typst reported success without producing an overlay PDF".to_string(),
            ));
        }
        Ok(TypstOutput {
            source_path,
            pdf_path: output_path,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypstOutput {
    pub source_path: PathBuf,
    pub pdf_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypstErrorKind {
    Cancelled,
    MissingAsset,
    Io,
    Process,
    Compile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypstError {
    pub kind: TypstErrorKind,
    pub message: String,
}

impl TypstError {
    fn cancelled() -> Self {
        Self {
            kind: TypstErrorKind::Cancelled,
            message: "PDF translation was cancelled".to_string(),
        }
    }

    fn missing_asset(message: impl Into<String>) -> Self {
        Self {
            kind: TypstErrorKind::MissingAsset,
            message: message.into(),
        }
    }

    fn io(message: impl Into<String>) -> Self {
        Self {
            kind: TypstErrorKind::Io,
            message: message.into(),
        }
    }

    fn process(message: impl Into<String>) -> Self {
        Self {
            kind: TypstErrorKind::Process,
            message: message.into(),
        }
    }

    fn compile(message: impl Into<String>) -> Self {
        Self {
            kind: TypstErrorKind::Compile,
            message: message.into(),
        }
    }
}

impl fmt::Display for TypstError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for TypstError {}

pub fn resolve_translation_assets_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("GLYPHST_PDF_TRANSLATION_ASSETS") {
        return PathBuf::from(path);
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(prefix) = executable.parent().and_then(Path::parent)
    {
        let packaged = prefix.join("share/glyphst/pdf-translation");
        if packaged.is_dir() {
            return packaged;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets")
}

pub fn generate_typst_source(layout: &LayoutDocument) -> String {
    let mut source = String::from(
        r#"#import "@preview/cmarker:0.1.8"
#import "@preview/mitex:0.2.6": mitex
#set text(font: "Source Han Serif SC", lang: "zh")
#let glyphst_fit_size(lo, hi, eps, fits) = {
  if hi - lo <= eps {
    lo
  } else {
    let mid = lo + (hi - lo) / 2
    if fits(mid) {
      glyphst_fit_size(mid, hi, eps, fits)
    } else {
      glyphst_fit_size(lo, mid, eps, fits)
    }
  }
}
#let glyphst_fit_leading(lo, hi, eps, fits) = {
  if hi - lo <= eps {
    lo
  } else {
    let mid = lo + (hi - lo) / 2
    if fits(mid) {
      glyphst_fit_leading(mid, hi, eps, fits)
    } else {
      glyphst_fit_leading(lo, mid, eps, fits)
    }
  }
}
#let glyphst_fit(body, max-size: 10pt, min-size: 6pt, emergency-min-size: 5pt, max-leading: 0.58em, min-leading: 0.40em, fit-height: none, first-line-indent: 0pt, justify: false, strict: true, diagnostic: "unknown") = {
  layout(size => {
    let allowed-height = if fit-height == none { size.height } else { calc.min(size.height, fit-height) }
    let render(text-size, leading) = block(width: size.width)[#{
      set text(size: text-size)
      set par(leading: leading, justify: justify)
      if first-line-indent > 0pt { h(first-line-indent) }
      body(text-size)
    }]
    let fits(text-size, leading) = measure(width: size.width, render(text-size, leading)).height <= allowed-height
    let emergency-leading = calc.max(0.20em, min-leading * 0.75)
    if not fits(emergency-min-size, emergency-leading) {
      panic("GLYPHST_OVERFLOW:" + diagnostic)
    }
    if strict and not fits(min-size, min-leading) {
      panic("GLYPHST_OVERFLOW:" + diagnostic)
    }
    let chosen-size = if fits(max-size, max-leading) {
      max-size
    } else if fits(min-size, min-leading) {
      glyphst_fit_size(min-size, max-size, 0.08pt, size => fits(size, min-leading))
    } else {
      glyphst_fit_size(emergency-min-size, min-size, 0.08pt, size => fits(size, emergency-leading))
    }
    let leading-floor = if fits(chosen-size, min-leading) { min-leading } else { emergency-leading }
    let leading-cap = if fits(chosen-size, max-leading) { max-leading } else { min-leading }
    let chosen-leading = if fits(chosen-size, leading-cap) {
      leading-cap
    } else {
      glyphst_fit_leading(leading-floor, leading-cap, 0.01em, leading => fits(chosen-size, leading))
    }
    render(chosen-size, chosen-leading)
  })
}
"#,
    );
    for (page_position, page) in layout.pages.iter().enumerate() {
        if page_position > 0 {
            source.push_str("#pagebreak()\n");
        }
        source.push_str(&format!(
            "#set page(width: {:.3}pt, height: {:.3}pt, margin: 0pt, fill: none)\n",
            page.geometry.width, page.geometry.height
        ));
        for (item_index, item) in page.items.iter().enumerate() {
            append_typst_layout_item(
                &mut source,
                page.page_index,
                item_index,
                item,
                layout.overflow_policy,
            );
        }
    }
    source
}

pub fn generate_translated_pdf_source(
    layout: &LayoutDocument,
    source_pdf_name: &str,
    background_colors: &[Vec<[u8; 3]>],
) -> String {
    let overlay = generate_typst_source(layout);
    let preamble_end = overlay.find("#set page(").unwrap_or(overlay.len());
    let mut source = overlay[..preamble_end].to_string();
    for (page_position, page) in layout.pages.iter().enumerate() {
        if page_position > 0 {
            source.push_str("#pagebreak()\n");
        }
        source.push_str(&format!(
            "#set page(width: {:.3}pt, height: {:.3}pt, margin: 0pt, fill: none)\n",
            page.geometry.width, page.geometry.height
        ));
        source.push_str(&format!(
            "#place(top + left, image(\"{}\", page: {}, width: {:.3}pt, height: {:.3}pt, fit: \"stretch\"))\n",
            escape_typst_string(source_pdf_name),
            page.page_index + 1,
            page.geometry.width,
            page.geometry.height
        ));
        for (item_index, item) in page.items.iter().enumerate() {
            let color = background_colors
                .get(page.page_index)
                .and_then(|colors| colors.get(item_index))
                .copied()
                .unwrap_or([255, 255, 255]);
            let fallback_regions = [item.source_bbox];
            let regions = if item.source_regions.is_empty() {
                fallback_regions.as_slice()
            } else {
                item.source_regions.as_slice()
            };
            for region in regions {
                source.push_str(&format!(
                    "#place(top + left, dx: {:.3}pt, dy: {:.3}pt, rect(width: {:.3}pt, height: {:.3}pt, fill: rgb({}, {}, {}), stroke: none))\n",
                    region.x0,
                    region.y0,
                    region.width(),
                    region.height(),
                    color[0],
                    color[1],
                    color[2]
                ));
            }
            append_typst_layout_item(
                &mut source,
                page.page_index,
                item_index,
                item,
                layout.overflow_policy,
            );
        }
    }
    source
}

fn append_typst_layout_item(
    source: &mut String,
    page_index: usize,
    item_index: usize,
    item: &LayoutItem,
    overflow_policy: OverflowPolicy,
) {
    let name = format!("glyphst_p{}_i{}", page_index + 1, item_index);
    let base_style = TextStyle {
        font_family: None,
        font_class: Some(item.font_class),
        font_size: Some(item.source_font_size_pt),
        bold: item.font_weight == crate::FontWeight::Bold,
        italic: item.italic,
        underline: item.underline,
        strikeout: item.strikeout,
        color_argb: Some(item.color_argb),
    };
    let fallback_runs = [StyledRun {
        text: item.text.clone(),
        style: None,
    }];
    let runs = if item.styled_runs.is_empty() {
        fallback_runs.as_slice()
    } else {
        item.styled_runs.as_slice()
    };
    source.push_str(&format!("#let {name}_content(text-size) = [\n"));
    let mut index = 0usize;
    while index < runs.len() {
        let run = &runs[index];
        let style = merged_text_style(&base_style, run.style.as_ref());
        if style.font_class == Some(FontFamilyClass::Math) {
            source.push_str("#box[");
            while index < runs.len() {
                let run = &runs[index];
                let style = merged_text_style(&base_style, run.style.as_ref());
                if style.font_class != Some(FontFamilyClass::Math) {
                    break;
                }
                append_typst_styled_run(source, run, &style, item);
                if source.ends_with('\n') {
                    source.pop();
                }
                index += 1;
            }
            source.push_str("]\n");
        } else {
            append_typst_styled_run(source, run, &style, item);
            index += 1;
        }
    }
    source.push_str("]\n");
    source.push_str(&format!(
        "#let {name}_body = block(width: {:.3}pt, height: {:.3}pt, inset: 0pt, clip: false)[#{{ glyphst_fit({name}_content, max-size: {:.3}pt, min-size: {:.3}pt, emergency-min-size: {:.3}pt, max-leading: {:.3}em, min-leading: {:.3}em, fit-height: {:.3}pt, first-line-indent: {:.3}pt, justify: {}, strict: {}, diagnostic: \"{}\") }}]\n",
        item.bbox.width().max(1.0),
        item.bbox.height().max(1.0),
        item.font_size_pt,
        item.minimum_font_size_pt,
        item.emergency_minimum_font_size_pt,
        item.leading_em,
        item.minimum_leading_em,
        item.bbox.height().max(1.0),
        item.first_line_indent_pt,
        item.justify,
        overflow_policy == OverflowPolicy::Strict,
        escape_typst_string(&item.block_id)
    ));
    source.push_str(&format!(
        "#context {{ place(top + left, dx: {:.3}pt, dy: {:.3}pt, {name}_body) }}\n",
        item.bbox.x0, item.bbox.y0
    ));
}

fn merged_text_style(base: &TextStyle, override_style: Option<&TextStyle>) -> TextStyle {
    let Some(override_style) = override_style else {
        return base.clone();
    };
    TextStyle {
        font_family: override_style
            .font_family
            .clone()
            .or_else(|| base.font_family.clone()),
        font_class: override_style.font_class.or(base.font_class),
        font_size: override_style.font_size.or(base.font_size),
        bold: override_style.bold,
        italic: override_style.italic,
        underline: override_style.underline,
        strikeout: override_style.strikeout,
        color_argb: override_style.color_argb.or(base.color_argb),
    }
}

fn append_typst_styled_run(
    source: &mut String,
    run: &StyledRun,
    style: &TextStyle,
    item: &LayoutItem,
) {
    if run.text.is_empty() {
        return;
    }
    let markdown = if item.preserve_line_breaks {
        run.text.replace('\n', "  \n")
    } else {
        run.text.clone()
    };
    let markdown = normalize_markdown_math(&markdown);
    let font = typst_font_name(style.font_class.unwrap_or(FontFamilyClass::Serif));
    let source_size = style
        .font_size
        .filter(|size| size.is_finite() && *size > 0.0)
        .unwrap_or(item.source_font_size_pt);
    let size_ratio = (source_size / item.source_font_size_pt.max(0.1)).clamp(0.55, 2.4);
    let [red, green, blue] = argb_to_rgb(style.color_argb.unwrap_or(0xff00_0000));
    let escaped = escape_typst_string(&markdown);
    let mut content = if style.font_class == Some(FontFamilyClass::Math) {
        format!("#text(\"{escaped}\")")
    } else {
        format!("#cmarker.render(\"{escaped}\", math: mitex)")
    };
    if style.strikeout {
        content = format!("#strike[{content}]");
    }
    if style.underline {
        content = format!("#underline[{content}]");
    }
    source.push_str(&format!(
        "#text(font: \"{}\", size: text-size * {:.4}, weight: \"{}\", style: \"{}\", fill: rgb({}, {}, {}))[{}]\n",
        font,
        size_ratio,
        if style.bold { "bold" } else { "regular" },
        if style.italic { "italic" } else { "normal" },
        red,
        green,
        blue,
        content
    ));
}

fn typst_font_name(font_class: FontFamilyClass) -> &'static str {
    match font_class {
        FontFamilyClass::SansSerif => "Noto Sans CJK SC",
        FontFamilyClass::Monospace => "Lilex",
        FontFamilyClass::Serif | FontFamilyClass::Math | FontFamilyClass::Unknown => {
            "Source Han Serif SC"
        }
    }
}

fn argb_to_rgb(argb: u32) -> [u8; 3] {
    [
        ((argb >> 16) & 0xff) as u8,
        ((argb >> 8) & 0xff) as u8,
        (argb & 0xff) as u8,
    ]
}

fn sample_layout_background_colors(
    source_pdf: &Path,
    layout: &LayoutDocument,
) -> Result<Vec<Vec<[u8; 3]>>, TypstError> {
    let source = Document::open(source_pdf).map_err(|error| {
        TypstError::io(format!(
            "failed to open source PDF {} for background sampling: {error}",
            source_pdf.display()
        ))
    })?;
    let mut pages = Vec::with_capacity(layout.pages.len());
    for page in &layout.pages {
        let source_page = source.load_page(page.page_index as i32).map_err(|error| {
            TypstError::io(format!(
                "failed to load source page {} for background sampling: {error}",
                page.page_index + 1
            ))
        })?;
        let pixmap = source_page
            .to_pixmap(&Matrix::IDENTITY, &Colorspace::device_rgb(), false, false)
            .map_err(|error| {
                TypstError::io(format!(
                    "failed to render source page {} for background sampling: {error}",
                    page.page_index + 1
                ))
            })?;
        pages.push(
            page.items
                .iter()
                .map(|item| sample_background_color(&pixmap, item.source_bbox))
                .collect(),
        );
    }
    Ok(pages)
}

fn sample_background_color(pixmap: &mupdf::Pixmap, region: BoundingBox) -> [u8; 3] {
    let (origin_x, origin_y) = pixmap.origin();
    let width = pixmap.width() as i32;
    let height = pixmap.height() as i32;
    let stride = pixmap.stride().max(0) as usize;
    let components = usize::from(pixmap.n());
    if width <= 0 || height <= 0 || components < 3 {
        return [255, 255, 255];
    }
    let x0 = (region.x0.floor() as i32 - origin_x).clamp(0, width - 1);
    let y0 = (region.y0.floor() as i32 - origin_y).clamp(0, height - 1);
    let x1 = (region.x1.ceil() as i32 - origin_x).clamp(0, width - 1);
    let y1 = (region.y1.ceil() as i32 - origin_y).clamp(0, height - 1);
    let samples = pixmap.samples();
    let mut channels = [Vec::new(), Vec::new(), Vec::new()];
    let margin = 3;
    for y in (y0 - margin).max(0)..=(y1 + margin).min(height - 1) {
        for x in (x0 - margin).max(0)..=(x1 + margin).min(width - 1) {
            if (x >= x0 && x <= x1 && y >= y0 && y <= y1) || (x + y) % 2 != 0 {
                continue;
            }
            let offset = y as usize * stride + x as usize * components;
            if offset + 2 < samples.len() {
                channels[0].push(samples[offset]);
                channels[1].push(samples[offset + 1]);
                channels[2].push(samples[offset + 2]);
            }
        }
    }
    if channels[0].is_empty() {
        return [255, 255, 255];
    }
    for channel in &mut channels {
        channel.sort_unstable();
    }
    [
        channels[0][channels[0].len() / 2],
        channels[1][channels[1].len() / 2],
        channels[2][channels[2].len() / 2],
    ]
}

fn escape_typst_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn normalize_markdown_math(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut normalized = String::with_capacity(value.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'$' || is_escaped(bytes, index) {
            let character = value[index..].chars().next().unwrap();
            normalized.push(character);
            index += character.len_utf8();
            continue;
        }

        let delimiter_len = usize::from(bytes.get(index + 1) == Some(&b'$')) + 1;
        let content_start = index + delimiter_len;
        let mut cursor = content_start;
        let closing = loop {
            if cursor + delimiter_len > bytes.len() {
                break None;
            }
            if bytes[cursor] == b'$'
                && !is_escaped(bytes, cursor)
                && (delimiter_len == 1 || bytes.get(cursor + 1) == Some(&b'$'))
            {
                break Some(cursor);
            }
            let character = value[cursor..].chars().next().unwrap();
            cursor += character.len_utf8();
        };
        let Some(closing) = closing else {
            normalized.push_str(&value[index..]);
            break;
        };
        let content = value[content_start..closing].trim();
        if content.is_empty() {
            normalized.push_str(&value[index..closing + delimiter_len]);
        } else {
            normalized.push_str(&"$".repeat(delimiter_len));
            normalized.push_str(content);
            normalized.push_str(&"$".repeat(delimiter_len));
        }
        index = closing + delimiter_len;
    }
    normalized
}

fn is_escaped(bytes: &[u8], index: usize) -> bool {
    let mut backslashes = 0;
    let mut cursor = index;
    while cursor > 0 && bytes[cursor - 1] == b'\\' {
        backslashes += 1;
        cursor -= 1;
    }
    backslashes % 2 == 1
}

fn validate_asset(path: &Path, label: &str) -> Result<(), TypstError> {
    if path.is_dir() {
        Ok(())
    } else {
        Err(TypstError::missing_asset(format!(
            "{label} is missing at {}",
            path.display()
        )))
    }
}

fn read_pipe(pipe: Option<impl Read>) -> String {
    let Some(mut pipe) = pipe else {
        return String::new();
    };
    let mut output = String::new();
    let _ = pipe.read_to_string(&mut output);
    output
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::{
        BoundingBox, FontWeight, LayoutItem, LayoutPage, PageGeometry, layout::LayoutDocument,
    };

    use super::*;

    #[test]
    fn generated_source_uses_offline_packages_and_fit_helpers() {
        let math_style = TextStyle {
            font_class: Some(FontFamilyClass::Math),
            font_size: Some(10.0),
            ..TextStyle::default()
        };
        let layout = LayoutDocument {
            pages: vec![LayoutPage {
                page_index: 0,
                geometry: PageGeometry::new(100.0, 200.0),
                items: vec![LayoutItem {
                    unit_id: "unit".to_string(),
                    block_id: "block".to_string(),
                    page_index: 0,
                    source_bbox: BoundingBox::new(10.0, 20.0, 90.0, 60.0),
                    source_regions: vec![BoundingBox::new(10.0, 20.0, 90.0, 60.0)],
                    bbox: BoundingBox::new(10.0, 20.0, 90.0, 60.0),
                    text: "你好 $E = mc^2$".to_string(),
                    role: crate::BlockRole::Body,
                    source_font_size_pt: 10.0,
                    font_size_pt: 10.0,
                    minimum_font_size_pt: 6.0,
                    emergency_minimum_font_size_pt: 5.5,
                    leading_em: 0.58,
                    minimum_leading_em: 0.4,
                    font_weight: FontWeight::Regular,
                    font_class: FontFamilyClass::Serif,
                    italic: false,
                    underline: false,
                    strikeout: false,
                    color_argb: 0xff00_0000,
                    styled_runs: vec![
                        StyledRun {
                            text: "你好 ".to_string(),
                            style: Some(TextStyle {
                                font_class: Some(FontFamilyClass::SansSerif),
                                font_size: Some(10.0),
                                bold: true,
                                italic: true,
                                color_argb: Some(0xff44_5566),
                                ..TextStyle::default()
                            }),
                        },
                        StyledRun {
                            text: "O(n".to_string(),
                            style: Some(math_style.clone()),
                        },
                        StyledRun {
                            text: "3".to_string(),
                            style: Some(TextStyle {
                                font_size: Some(8.0),
                                ..math_style.clone()
                            }),
                        },
                        StyledRun {
                            text: "+n2d)".to_string(),
                            style: Some(math_style),
                        },
                    ],
                    first_line_indent_pt: 0.0,
                    justify: true,
                    preserve_line_breaks: false,
                }],
            }],
            warnings: Vec::new(),
            preserved_on_overflow: Vec::new(),
            overflow_policy: OverflowPolicy::Strict,
        };

        let source = generate_typst_source(&layout);

        assert!(source.contains("@preview/cmarker:0.1.8"));
        assert!(source.contains("@preview/mitex:0.2.6"));
        assert!(source.contains("glyphst_fit"));
        assert!(source.contains("fill: none"));
        assert!(source.contains("clip: false"));
        assert!(source.contains("font: \"Noto Sans CJK SC\""));
        assert!(source.contains("weight: \"bold\""));
        assert!(source.contains("style: \"italic\""));
        assert!(source.contains("fill: rgb(68, 85, 102)"));
        assert!(source.contains("#box["));
        assert!(source.contains("#text(\"+n2d)\")"));
        assert!(!source.contains("cmarker.render(\"+n2d)\""));
        assert!(!source.contains("#text(\"O(n\")\n#text(\"3\")"));
        assert!(!source.contains("clip: true"));
    }

    #[test]
    fn trims_inline_math_delimiter_whitespace_for_cmarker() {
        assert_eq!(
            normalize_markdown_math("比较 $ \\sigma $ 与 $ O(n^2) $。"),
            "比较 $\\sigma$ 与 $O(n^2)$。"
        );
        assert_eq!(
            normalize_markdown_math("价格为 $ 10，公式未闭合"),
            "价格为 $ 10，公式未闭合"
        );
    }

    #[test]
    fn compiles_overlay_with_bundled_assets_when_requested() {
        if std::env::var_os("GLYPHST_RUN_TYPST_TEST").is_none() {
            return;
        }
        let layout = LayoutDocument {
            pages: vec![LayoutPage {
                page_index: 0,
                geometry: PageGeometry::new(200.0, 200.0),
                items: vec![LayoutItem {
                    unit_id: "unit".to_string(),
                    block_id: "block".to_string(),
                    page_index: 0,
                    source_bbox: BoundingBox::new(10.0, 10.0, 190.0, 100.0),
                    source_regions: vec![BoundingBox::new(10.0, 10.0, 190.0, 100.0)],
                    bbox: BoundingBox::new(10.0, 10.0, 190.0, 100.0),
                    text: "简体中文第一行不应裁切。\nDansk: Ålborg, dejlig og lang.\nDeutsch: Ältere Übung, deutlich und lang. $E = mc^2$.".to_string(),
                    role: crate::BlockRole::Body,
                    source_font_size_pt: 10.0,
                    font_size_pt: 10.0,
                    minimum_font_size_pt: 6.0,
                    emergency_minimum_font_size_pt: 5.5,
                    leading_em: 0.58,
                    minimum_leading_em: 0.4,
                    font_weight: FontWeight::Regular,
                    font_class: FontFamilyClass::Serif,
                    italic: false,
                    underline: false,
                    strikeout: false,
                    color_argb: 0xff00_0000,
                    styled_runs: Vec::new(),
                    first_line_indent_pt: 0.0,
                    justify: true,
                    preserve_line_breaks: true,
                }],
            }],
            warnings: Vec::new(),
            preserved_on_overflow: Vec::new(),
            overflow_policy: OverflowPolicy::Strict,
        };
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("glyphst-typst-test-{nonce:x}"));

        let output = TypstCompiler::new(TypstCompilerConfig::default())
            .compile(&layout, &directory, &CancellationToken::new())
            .unwrap();

        assert!(output.pdf_path.is_file());
        let _ = fs::remove_dir_all(directory);
    }
}

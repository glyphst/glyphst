use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::JoinHandle;
use std::time::Duration;

use mupdf::{Colorspace, Document, ImageFormat, Matrix, TextPageFlags, text_page::TextBlockType};
use serde::{Deserialize, Serialize};
use typst::diag::{FileError, FileResult, SourceDiagnostic};
use typst::foundations::{Bytes, Datetime, Duration as TypstDuration};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_kit::diagnostics::{DiagnosticFormat, DiagnosticWorld, emit, termcolor::NoColor};
use typst_kit::files::{FileLoader, FileStore};
use typst_kit::fonts::{self, FontStore};
use typst_layout::PagedDocument;
use typst_pdf::PdfOptions;

use crate::{
    BaselinePosition, BoundingBox, CancellationToken, FontFamilyClass, LayoutDocument, LayoutItem,
    OverflowPolicy, StyledRun, TextStyle, mupdf_path,
};

const TYPST_WORKER_FLAG: &str = "--glyphst-typst-worker";
const TYPST_WORKER_PROTOCOL_VERSION: u32 = 1;
const TYPST_WORKER_OUTPUT_LIMIT: usize = 64 * 1024;
const CMARKER_PACKAGE: &str = "@preview/cmarker:0.1.8";
const MITEX_PACKAGE: &str = "@preview/mitex:0.2.6";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TypstExecutionMode {
    #[default]
    InProcess,
    CurrentExecutableWorker,
}

#[derive(Debug, Clone)]
pub struct TypstCompilerConfig {
    pub assets_dir: PathBuf,
    /// The glyphst-fonts checkout whose `fonts/` holds the CJK and Lilex faces.
    pub fonts_dir: PathBuf,
    pub execution: TypstExecutionMode,
}

impl Default for TypstCompilerConfig {
    fn default() -> Self {
        Self {
            assets_dir: resolve_translation_assets_dir(),
            fonts_dir: glyphst_core::paths::fonts_dir().unwrap_or_default(),
            execution: TypstExecutionMode::InProcess,
        }
    }
}

impl TypstCompilerConfig {
    pub fn current_executable_worker() -> Self {
        Self {
            execution: TypstExecutionMode::CurrentExecutableWorker,
            ..Self::default()
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
        original_pdf: &Path,
        source_pdf: &Path,
        layout: &LayoutDocument,
        bases: bool,
        job_directory: &Path,
        cancellation: &CancellationToken,
    ) -> Result<TypstOutput, TypstError> {
        fs::create_dir_all(job_directory).map_err(|error| {
            TypstError::io(format!(
                "failed to create Typst job directory {}: {error}",
                job_directory.display()
            ))
        })?;
        materialize_inline_assets(original_pdf, layout, job_directory)?;
        let copied_source = job_directory.join("source.pdf");
        fs::copy(source_pdf, &copied_source).map_err(|error| {
            TypstError::io(format!(
                "failed to stage source PDF {}: {error}",
                source_pdf.display()
            ))
        })?;
        let colors = sample_layout_background_colors(source_pdf, layout)?;
        self.compile_source(
            generate_translated_pdf_source(layout, "source.pdf", &colors, bases),
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

        let result = match self.config.execution {
            TypstExecutionMode::InProcess => compile_source_in_process(
                job_directory,
                &self.config.assets_dir,
                &self.config.fonts_dir,
                file_stem,
            ),
            TypstExecutionMode::CurrentExecutableWorker => self.compile_source_in_worker(
                job_directory,
                file_stem,
                &source_path,
                &output_path,
                cancellation,
            ),
        };
        cancellation.check().map_err(|_| TypstError::cancelled())?;
        result
    }

    fn compile_source_in_worker(
        &self,
        job_directory: &Path,
        file_stem: &str,
        source_path: &Path,
        output_path: &Path,
        cancellation: &CancellationToken,
    ) -> Result<TypstOutput, TypstError> {
        validate_file_stem(file_stem)?;
        validate_compiler_assets(&self.config.assets_dir, &self.config.fonts_dir)?;
        let executable = std::env::current_exe().map_err(|error| {
            TypstError::process(format!(
                "failed to locate the Glyphst Reader executable for the Typst worker: {error}"
            ))
        })?;
        let mut child = Command::new(&executable)
            .arg(TYPST_WORKER_FLAG)
            .arg(job_directory)
            .arg(&self.config.assets_dir)
            .arg(&self.config.fonts_dir)
            .arg(file_stem)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                TypstError::process(format!(
                    "failed to start the Glyphst Reader Typst worker {}: {error}",
                    executable.display()
                ))
            })?;

        let stdout_reader = spawn_pipe_reader(child.stdout.take());
        let stderr_reader = spawn_pipe_reader(child.stderr.take());
        let status = loop {
            if cancellation.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                let _ = fs::remove_file(output_path.with_extension("pdf.part"));
                return Err(TypstError::cancelled());
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    let _ = fs::remove_file(output_path.with_extension("pdf.part"));
                    return Err(TypstError::process(format!(
                        "failed while waiting for the Glyphst Reader Typst worker: {error}"
                    )));
                }
            }
        };

        let stdout = join_pipe_reader(stdout_reader, "stdout")?;
        let stderr = join_pipe_reader(stderr_reader, "stderr")?;
        let response =
            serde_json::from_str::<TypstWorkerResponse>(stdout.trim()).map_err(|error| {
                TypstError::process(worker_failure_message(
                    format!("the worker returned an invalid response: {error}"),
                    &stdout,
                    &stderr,
                ))
            })?;
        if response.protocol_version != TYPST_WORKER_PROTOCOL_VERSION {
            return Err(TypstError::process(format!(
                "unsupported Glyphst Reader Typst worker protocol version {}",
                response.protocol_version
            )));
        }
        if let Some(error) = response.error {
            return Err(error);
        }
        if !status.success() {
            return Err(TypstError::process(worker_failure_message(
                format!("the worker exited with status {status}"),
                &stdout,
                &stderr,
            )));
        }
        if !output_path.is_file() {
            return Err(TypstError::compile(
                "the embedded Typst compiler reported success without producing a PDF".to_string(),
            ));
        }
        Ok(TypstOutput {
            source_path: source_path.to_path_buf(),
            pdf_path: output_path.to_path_buf(),
        })
    }
}

fn materialize_inline_assets(
    original_pdf: &Path,
    layout: &LayoutDocument,
    job_directory: &Path,
) -> Result<(), TypstError> {
    let mut requests = BTreeMap::<usize, Vec<(usize, String)>>::new();
    for page in &layout.pages {
        for item in &page.items {
            for object in &item.inline_objects {
                for (part_index, part) in object.parts.iter().enumerate() {
                    let request = (
                        part.source_block_index,
                        inline_asset_filename(&object.id, part_index),
                    );
                    let page_requests = requests.entry(object.page_index).or_default();
                    if !page_requests.contains(&request) {
                        page_requests.push(request);
                    }
                }
            }
        }
    }
    if requests.is_empty() {
        return Ok(());
    }

    let document = mupdf_path(original_pdf)
        .and_then(Document::open)
        .map_err(|error| {
            TypstError::io(format!(
                "failed to open original PDF {} for inline images: {error}",
                original_pdf.display()
            ))
        })?;
    for (page_index, page_requests) in requests {
        let page = document.load_page(page_index as i32).map_err(|error| {
            TypstError::io(format!(
                "failed to load source page {} for inline images: {error}",
                page_index + 1
            ))
        })?;
        let text_page = page
            .to_text_page(
                TextPageFlags::PRESERVE_WHITESPACE
                    | TextPageFlags::ACCURATE_BBOXES
                    | TextPageFlags::ACCURATE_ASCENDERS
                    | TextPageFlags::ACCURATE_SIDE_BEARINGS
                    | TextPageFlags::PRESERVE_SPANS
                    | TextPageFlags::PRESERVE_IMAGES
                    | TextPageFlags::COLLECT_STYLES,
            )
            .map_err(|error| {
                TypstError::io(format!(
                    "failed to inspect source page {} for inline images: {error}",
                    page_index + 1
                ))
            })?;
        let mut remaining = page_requests
            .into_iter()
            .collect::<BTreeMap<usize, String>>();
        for (source_block_index, block) in text_page.blocks().enumerate() {
            let Some(filename) = remaining.remove(&source_block_index) else {
                continue;
            };
            if block.r#type() != TextBlockType::Image {
                return Err(TypstError::missing_asset(format!(
                    "source block {source_block_index} on page {} is not an inline image",
                    page_index + 1
                )));
            }
            let image = block.image().ok_or_else(|| {
                TypstError::missing_asset(format!(
                    "source block {source_block_index} on page {} has no image payload",
                    page_index + 1
                ))
            })?;
            let pixmap = image.to_pixmap().map_err(|error| {
                TypstError::io(format!(
                    "failed to decode inline image on page {}: {error}",
                    page_index + 1
                ))
            })?;
            let path = job_directory.join(filename);
            let path_string = path.to_string_lossy();
            pixmap
                .save_as(path_string.as_ref(), ImageFormat::PNG)
                .map_err(|error| {
                    TypstError::io(format!(
                        "failed to save inline image {}: {error}",
                        path.display()
                    ))
                })?;
        }
        if let Some((source_block_index, _)) = remaining.into_iter().next() {
            return Err(TypstError::missing_asset(format!(
                "could not find source inline image block {source_block_index} on page {}",
                page_index + 1
            )));
        }
    }
    Ok(())
}

fn inline_asset_filename(object_id: &str, part_index: usize) -> String {
    let id = object_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("inline-{id}-{part_index:02}.png")
}

#[derive(Debug, Serialize, Deserialize)]
struct TypstWorkerResponse {
    protocol_version: u32,
    error: Option<TypstError>,
}

/// Handles Glyphst Reader's private Typst worker invocation before the desktop
/// application initializes. Returns `None` for normal application invocations.
pub fn run_typst_worker_if_requested() -> Option<i32> {
    let mut arguments = std::env::args_os();
    let _executable = arguments.next();
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new(TYPST_WORKER_FLAG)) {
        return None;
    }

    let result = (|| {
        let job_directory = arguments
            .next()
            .ok_or_else(|| TypstError::process("Typst worker is missing its job directory"))?;
        let assets_dir = arguments
            .next()
            .ok_or_else(|| TypstError::process("Typst worker is missing its assets directory"))?;
        let fonts_dir = arguments
            .next()
            .ok_or_else(|| TypstError::process("Typst worker is missing its fonts directory"))?;
        let file_stem = arguments
            .next()
            .ok_or_else(|| TypstError::process("Typst worker is missing its source file stem"))?;
        if arguments.next().is_some() {
            return Err(TypstError::process(
                "Typst worker received unexpected arguments",
            ));
        }
        let file_stem = file_stem
            .to_str()
            .ok_or_else(|| TypstError::process("Typst worker file stem is not valid UTF-8"))?;
        compile_source_in_process(
            Path::new(&job_directory),
            Path::new(&assets_dir),
            Path::new(&fonts_dir),
            file_stem,
        )
    })();

    let exit_code = if result.is_ok() { 0 } else { 1 };
    let response = TypstWorkerResponse {
        protocol_version: TYPST_WORKER_PROTOCOL_VERSION,
        error: result.err(),
    };
    let mut stdout = io::stdout().lock();
    if serde_json::to_writer(&mut stdout, &response).is_err()
        || stdout.write_all(b"\n").is_err()
        || stdout.flush().is_err()
    {
        return Some(2);
    }
    Some(exit_code)
}

fn compile_source_in_process(
    job_directory: &Path,
    assets_dir: &Path,
    fonts_dir: &Path,
    file_stem: &str,
) -> Result<TypstOutput, TypstError> {
    validate_file_stem(file_stem)?;
    validate_compiler_assets(assets_dir, fonts_dir)?;
    let source_path = job_directory.join(format!("{file_stem}.typ"));
    validate_file_asset(&source_path, "generated Typst source")?;

    let world = GlyphstTypstWorld::new(job_directory, assets_dir, fonts_dir, file_stem)?;
    let compilation = typst::compile::<PagedDocument>(&world);
    if !compilation.warnings.is_empty() {
        tracing::warn!(
            diagnostics = %format_typst_diagnostics(&world, &compilation.warnings),
            "embedded Typst compilation produced warnings"
        );
    }
    let document = compilation.output.map_err(|diagnostics| {
        TypstError::compile(format_typst_diagnostics(&world, &diagnostics))
    })?;
    let pdf = typst_pdf::pdf(&document, &PdfOptions::default()).map_err(|diagnostics| {
        TypstError::compile(format_typst_diagnostics(&world, &diagnostics))
    })?;
    let output_path = job_directory.join(format!("{file_stem}.pdf"));
    write_pdf_atomically(&output_path, &pdf)?;

    Ok(TypstOutput {
        source_path,
        pdf_path: output_path,
    })
}

struct GlyphstTypstWorld {
    library: LazyHash<Library>,
    fonts: FontStore,
    main: FileId,
    files: FileStore<RestrictedFileLoader>,
}

impl GlyphstTypstWorld {
    fn new(
        job_directory: &Path,
        assets_dir: &Path,
        fonts_dir: &Path,
        file_stem: &str,
    ) -> Result<Self, TypstError> {
        let font_path = fonts_dir.join("fonts");
        let font_entries = fonts::scan(&font_path).collect::<Vec<_>>();
        if font_entries.is_empty() {
            return Err(TypstError::missing_asset(format!(
                "translation font directory contains no readable fonts at {}",
                font_path.display()
            )));
        }
        let mut font_store = FontStore::new();
        font_store.extend(fonts::embedded());
        font_store.extend(font_entries);

        let main_name = format!("{file_stem}.typ");
        let main_path = VirtualPath::new(&main_name).map_err(|error| {
            TypstError::io(format!("failed to create Typst source path: {error}"))
        })?;
        let main = RootedPath::new(VirtualRoot::Project, main_path).intern();
        let loader = RestrictedFileLoader::new(job_directory, assets_dir, main_name)?;

        Ok(Self {
            library: LazyHash::new(Library::builder().build()),
            fonts: font_store,
            main,
            files: FileStore::new(loader),
        })
    }
}

impl World for GlyphstTypstWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        self.fonts.book()
    }

    fn main(&self) -> FileId {
        self.main
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        self.files.source(id)
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        self.files.file(id)
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.font(index)
    }

    fn today(&self, _offset: Option<TypstDuration>) -> Option<Datetime> {
        None
    }
}

impl DiagnosticWorld for GlyphstTypstWorld {
    fn name(&self, id: FileId) -> String {
        match id.root() {
            VirtualRoot::Project => id.vpath().get_without_slash().to_string(),
            VirtualRoot::Package(package) => {
                format!("{package}{}", id.vpath().get_with_slash())
            }
        }
    }
}

struct RestrictedFileLoader {
    project: RestrictedRoot,
    main_name: String,
    cmarker: RestrictedRoot,
    mitex: RestrictedRoot,
}

impl RestrictedFileLoader {
    fn new(job_directory: &Path, assets_dir: &Path, main_name: String) -> Result<Self, TypstError> {
        let packages = assets_dir.join("typst/packages/preview");
        Ok(Self {
            project: RestrictedRoot::new(job_directory, "Typst job directory")?,
            main_name,
            cmarker: RestrictedRoot::new(packages.join("cmarker/0.1.8"), "cmarker Typst package")?,
            mitex: RestrictedRoot::new(packages.join("mitex/0.2.6"), "mitex Typst package")?,
        })
    }
}

impl FileLoader for RestrictedFileLoader {
    fn load(&self, id: FileId) -> FileResult<Bytes> {
        match id.root() {
            VirtualRoot::Project => {
                let path = id.vpath().get_without_slash();
                if path != self.main_name && path != "source.pdf" && !is_inline_asset_path(path) {
                    return Err(FileError::AccessDenied);
                }
                self.project.load(id.vpath())
            }
            VirtualRoot::Package(package) => match package.to_string().as_str() {
                CMARKER_PACKAGE => self.cmarker.load(id.vpath()),
                MITEX_PACKAGE => self.mitex.load(id.vpath()),
                _ => Err(FileError::Other(Some(
                    format!("package {package} is not bundled with Glyphst Reader").into(),
                ))),
            },
        }
    }
}

fn is_inline_asset_path(path: &str) -> bool {
    path.starts_with("inline-")
        && path.ends_with(".png")
        && !path.contains('/')
        && !path.contains('\\')
        && path.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
}

struct RestrictedRoot {
    path: PathBuf,
    canonical_path: PathBuf,
}

impl RestrictedRoot {
    fn new(path: impl AsRef<Path>, label: &str) -> Result<Self, TypstError> {
        let path = path.as_ref();
        validate_asset(path, label)?;
        let canonical_path = fs::canonicalize(path).map_err(|error| {
            TypstError::io(format!(
                "failed to resolve {label} {}: {error}",
                path.display()
            ))
        })?;
        Ok(Self {
            path: path.to_path_buf(),
            canonical_path,
        })
    }

    fn load(&self, virtual_path: &VirtualPath) -> FileResult<Bytes> {
        let path = virtual_path
            .realize(&self.path)
            .map_err(FileError::Realize)?;
        let canonical_path =
            fs::canonicalize(&path).map_err(|error| FileError::from_io(error, &path))?;
        if !canonical_path.starts_with(&self.canonical_path) {
            return Err(FileError::AccessDenied);
        }
        let metadata = fs::metadata(&canonical_path)
            .map_err(|error| FileError::from_io(error, &canonical_path))?;
        if metadata.is_dir() {
            return Err(FileError::IsDirectory);
        }
        fs::read(&canonical_path)
            .map(Bytes::new)
            .map_err(|error| FileError::from_io(error, &canonical_path))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypstOutput {
    pub source_path: PathBuf,
    pub pdf_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypstErrorKind {
    Cancelled,
    MissingAsset,
    Io,
    Process,
    Compile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
        let packaged = prefix.join("share/glyphst-reader/pdf-translation");
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
    let size-floor = if strict { min-size } else { calc.min(emergency-min-size, 1pt) }
    let leading-floor = if strict { min-leading } else { 0em }
    if not fits(size-floor, leading-floor) {
      panic("GLYPHST_OVERFLOW:" + diagnostic)
    }
    // Text keeps its own line spacing while it shrinks to its minimum
    // size; only then do its lines close up, to their minimum leading; then
    // (adaptive only) it shrinks further; leading goes under the minimum
    // only when even the smallest size needs it. Lines never crowd while a
    // smaller size would fit.
    let (chosen-size, chosen-leading) = if fits(max-size, max-leading) {
      (max-size, max-leading)
    } else if fits(min-size, max-leading) {
      (glyphst_fit_size(min-size, max-size, 0.08pt, size => fits(size, max-leading)), max-leading)
    } else if fits(min-size, min-leading) {
      (min-size, glyphst_fit_leading(min-leading, max-leading, 0.01em, leading => fits(min-size, leading)))
    } else if fits(size-floor, min-leading) {
      (glyphst_fit_size(size-floor, min-size, 0.08pt, size => fits(size, min-leading)), min-leading)
    } else {
      (size-floor, glyphst_fit_leading(leading-floor, min-leading, 0.01em, leading => fits(size-floor, leading)))
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

/// The Typst source of the translated document: each source page (with
/// its translated regions removed) under its translation. With `bases`,
/// each page is followed by its base: the same page without the
/// translation, which reveals draw where a translated glyph has not
/// arrived yet.
pub fn generate_translated_pdf_source(
    layout: &LayoutDocument,
    source_pdf_name: &str,
    background_colors: &[Vec<[u8; 3]>],
    bases: bool,
) -> String {
    let overlay = generate_typst_source(layout);
    let preamble_end = overlay.find("#set page(").unwrap_or(overlay.len());
    let mut source = overlay[..preamble_end].to_string();
    for (page_position, page) in layout.pages.iter().enumerate() {
        for translated in std::iter::once(true).chain(bases.then_some(false)) {
            if page_position > 0 || !translated {
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
                if translated {
                    append_typst_layout_item(
                        &mut source,
                        page.page_index,
                        item_index,
                        item,
                        layout.overflow_policy,
                    );
                }
            }
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
        baseline_position: BaselinePosition::Normal,
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
    source.push_str(&format!("#let {name}_content(text-size) = ["));
    if let Some(toc) = &item.toc {
        if let Some(marker) = toc
            .marker
            .as_deref()
            .filter(|marker| !marker.trim().is_empty())
        {
            append_typst_styled_text(source, marker, &base_style, item, true);
            source.push_str("#h(0.55em)");
        }
        append_typst_runs(source, runs, &base_style, item);
        source.push_str("#h(0.45em)");
        if let Some(leader) = &toc.leader {
            source.push_str(&format!(
                "#box(width: 1fr, repeat(text(\"{}\"), gap: {:.3}em))",
                escape_typst_string(&leader.glyph),
                leader.gap_em.clamp(0.0, 2.0)
            ));
        } else {
            source.push_str("#box(width: 1fr)[]");
        }
        source.push_str("#h(0.35em)");
        append_typst_styled_text(source, &toc.page_label, &base_style, item, true);
    } else {
        append_typst_runs(source, runs, &base_style, item);
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

fn append_typst_runs(
    source: &mut String,
    runs: &[StyledRun],
    base_style: &TextStyle,
    item: &LayoutItem,
) {
    let mut index = 0usize;
    while index < runs.len() {
        let run = &runs[index];
        let style = merged_text_style(base_style, run.style.as_ref());
        if let Some(end) = formula_cluster_end(runs, base_style, index) {
            append_typst_formula_cluster(source, &runs[index..end], base_style, item);
            index = end;
        } else if is_formula_anchor(&style) {
            source.push_str("#box[");
            while index < runs.len() {
                let run = &runs[index];
                let style = merged_text_style(base_style, run.style.as_ref());
                if !is_formula_anchor(&style) {
                    break;
                }
                append_typst_styled_run(source, run, &style, item, true);
                index += 1;
            }
            source.push(']');
        } else {
            append_typst_styled_run(source, run, &style, item, false);
            index += 1;
        }
    }
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
        baseline_position: override_style.baseline_position,
    }
}

fn is_formula_anchor(style: &TextStyle) -> bool {
    style.font_class == Some(FontFamilyClass::Math)
        || style.baseline_position != BaselinePosition::Normal
}

fn formula_cluster_end(runs: &[StyledRun], base_style: &TextStyle, start: usize) -> Option<usize> {
    let first_style = merged_text_style(base_style, runs.get(start)?.style.as_ref());
    if !is_formula_anchor(&first_style) {
        return None;
    }

    let mut end = start + 1;
    let mut anchors = 1usize;
    while end < runs.len() {
        let style = merged_text_style(base_style, runs[end].style.as_ref());
        if is_formula_anchor(&style) {
            anchors += 1;
            end += 1;
            continue;
        }
        if !is_formula_bridge(&runs[end].text) {
            break;
        }

        let bridge_start = end;
        while end < runs.len() {
            let style = merged_text_style(base_style, runs[end].style.as_ref());
            if is_formula_anchor(&style) || !is_formula_bridge(&runs[end].text) {
                break;
            }
            end += 1;
        }
        if end >= runs.len()
            || !is_formula_anchor(&merged_text_style(base_style, runs[end].style.as_ref()))
        {
            end = bridge_start;
            break;
        }
    }

    (anchors > 1 || end > start + 1).then_some(end)
}

fn is_formula_bridge(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|character| {
            character.is_whitespace()
                || matches!(
                    character,
                    '+' | '-'
                        | '−'
                        | '±'
                        | '∓'
                        | '='
                        | '≠'
                        | '<'
                        | '>'
                        | '≤'
                        | '≥'
                        | '≈'
                        | '≃'
                        | '∼'
                        | '~'
                        | '·'
                        | '⋅'
                        | '×'
                        | '÷'
                        | '/'
                        | '\\'
                        | '*'
                        | '^'
                        | '_'
                        | '%'
                        | ':'
                        | ';'
                        | ','
                        | '.'
                        | '('
                        | ')'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                        | '|'
                        | '‖'
                        | '&'
                        | '!'
                        | '?'
                )
        })
}

fn is_formula_break_bridge(text: &str) -> bool {
    text.chars().any(|character| {
        matches!(
            character,
            '+' | '-'
                | '−'
                | '±'
                | '∓'
                | '='
                | '≠'
                | '<'
                | '>'
                | '≤'
                | '≥'
                | '≈'
                | '≃'
                | '∼'
                | '~'
        )
    })
}

fn append_typst_formula_cluster(
    source: &mut String,
    runs: &[StyledRun],
    base_style: &TextStyle,
    item: &LayoutItem,
) {
    let mut chunk_start = 0usize;
    let mut previous_was_anchor = true;
    for index in 1..runs.len() {
        let style = merged_text_style(base_style, runs[index].style.as_ref());
        if is_formula_anchor(&style) {
            previous_was_anchor = true;
        } else {
            if previous_was_anchor && is_formula_break_bridge(&runs[index].text) {
                append_typst_formula_chunk(
                    source,
                    &runs[chunk_start..index],
                    base_style,
                    item,
                    chunk_start > 0,
                );
                chunk_start = index;
            }
            previous_was_anchor = false;
        }
    }
    append_typst_formula_chunk(
        source,
        &runs[chunk_start..],
        base_style,
        item,
        chunk_start > 0,
    );
}

fn append_typst_formula_chunk(
    source: &mut String,
    runs: &[StyledRun],
    base_style: &TextStyle,
    item: &LayoutItem,
    break_before: bool,
) {
    if runs.is_empty() {
        return;
    }

    let first_style = merged_text_style(base_style, runs[0].style.as_ref());
    let first_text = if break_before {
        let content_start = runs[0]
            .text
            .find(|character: char| !character.is_whitespace())
            .unwrap_or(runs[0].text.len());
        let (leading, content) = runs[0].text.split_at(content_start);
        if !leading.is_empty() {
            append_typst_styled_run_text(source, leading, &first_style, item, true);
        } else {
            source.push_str("#text(\"\u{200B}\")");
        }
        content
    } else {
        runs[0].text.as_str()
    };

    source.push_str("#box[");
    append_typst_styled_run_text(source, first_text, &first_style, item, true);
    for run in &runs[1..] {
        let style = merged_text_style(base_style, run.style.as_ref());
        append_typst_styled_run(source, run, &style, item, true);
    }
    source.push(']');
}

fn append_typst_styled_run(
    source: &mut String,
    run: &StyledRun,
    style: &TextStyle,
    item: &LayoutItem,
    literal: bool,
) {
    append_typst_styled_run_text(source, &run.text, style, item, literal);
}

fn append_typst_styled_run_text(
    source: &mut String,
    run_text: &str,
    style: &TextStyle,
    item: &LayoutItem,
    literal: bool,
) {
    if run_text.is_empty() {
        return;
    }
    let mut remaining = run_text;
    while let Some(start) = remaining.find(crate::document::INLINE_OBJECT_MARKER_START) {
        append_typst_plain_styled_run_text(source, &remaining[..start], style, item, literal);
        let marker = &remaining[start..];
        let Some((length, object_id)) = crate::document::inline_object_marker_prefix(marker) else {
            append_typst_plain_styled_run_text(source, marker, style, item, literal);
            return;
        };
        append_typst_inline_object(source, object_id, item);
        remaining = &marker[length..];
    }
    append_typst_plain_styled_run_text(source, remaining, style, item, literal);
}

fn append_typst_plain_styled_run_text(
    source: &mut String,
    run_text: &str,
    style: &TextStyle,
    item: &LayoutItem,
    literal: bool,
) {
    if run_text.is_empty() {
        return;
    }
    let parts = if item.preserve_line_breaks {
        run_text.split('\n').collect::<Vec<_>>()
    } else {
        vec![run_text]
    };
    for (index, part) in parts.into_iter().enumerate() {
        if index > 0 {
            source.push_str("#linebreak()");
        }
        let text = if item.preserve_line_breaks {
            part.to_string()
        } else {
            part.replace('\n', " ")
        };
        if text.is_empty() {
            continue;
        }
        append_typst_styled_text(source, &text, style, item, literal);
    }
}

fn append_typst_inline_object(source: &mut String, object_id: &str, item: &LayoutItem) {
    let Some(object) = item
        .inline_objects
        .iter()
        .find(|object| object.id == object_id)
    else {
        return;
    };
    let source_font_size = item.source_font_size_pt.max(0.1);
    let width_ratio = object.bbox.width() / source_font_size;
    let height_ratio = object.bbox.height() / source_font_size;
    // Typst's bare `baseline: <length>` value is a shift from the box's
    // automatically selected baseline, not an absolute position within the
    // box. Anchor at the bottom explicitly, then shift to the PDF text
    // baseline. Most inline images sit exactly on that baseline, so their
    // shift is zero and their bottom aligns with the surrounding glyphs.
    let baseline_shift_ratio = ((object.bbox.y1 - object.baseline_y) / source_font_size)
        .clamp(-height_ratio, height_ratio);
    source.push_str(&format!(
        "#box(width: text-size * {:.5}, height: text-size * {:.5}, baseline: (at: bottom, shift: text-size * {:.5}))[",
        width_ratio, height_ratio, baseline_shift_ratio
    ));
    for (part_index, part) in object.parts.iter().enumerate() {
        source.push_str(&format!(
            "#place(top + left, dx: text-size * {:.5}, dy: text-size * {:.5}, image(\"{}\", width: text-size * {:.5}, height: text-size * {:.5}, fit: \"stretch\"))",
            (part.bbox.x0 - object.bbox.x0) / source_font_size,
            (part.bbox.y0 - object.bbox.y0) / source_font_size,
            escape_typst_string(&inline_asset_filename(&object.id, part_index)),
            part.bbox.width() / source_font_size,
            part.bbox.height() / source_font_size,
        ));
    }
    source.push(']');
}

fn append_typst_styled_text(
    source: &mut String,
    text: &str,
    style: &TextStyle,
    item: &LayoutItem,
    literal: bool,
) {
    let font = typst_font_name(style.font_class.unwrap_or(FontFamilyClass::Serif));
    let source_size = style
        .font_size
        .filter(|size| size.is_finite() && *size > 0.0)
        .unwrap_or(item.source_font_size_pt);
    let size_ratio = if style.baseline_position == BaselinePosition::Normal {
        (source_size / item.source_font_size_pt.max(0.1)).clamp(0.55, 2.4)
    } else {
        1.0
    };
    let [red, green, blue] = argb_to_rgb(style.color_argb.unwrap_or(0xff00_0000));
    let mut content = typst_literal_with_math(
        text,
        !literal && style.font_class != Some(FontFamilyClass::Math),
    );
    if style.strikeout {
        content = format!("#strike[{content}]");
    }
    if style.underline {
        content = format!("#underline[{content}]");
    }
    if style.baseline_position == BaselinePosition::Superscript {
        content = format!("#super[{content}]");
    } else if style.baseline_position == BaselinePosition::Subscript {
        content = format!("#sub[{content}]");
    }
    source.push_str(&format!(
        "#text(font: \"{}\", size: text-size * {:.4}, weight: \"{}\", style: \"{}\", fill: rgb({}, {}, {}))[{}]",
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

fn typst_literal_with_math(value: &str, recognize_math: bool) -> String {
    if !recognize_math {
        return format!("#text(\"{}\")", escape_typst_string(value));
    }
    let spans = recognized_math_spans(value);
    if spans.iter().all(|span| !span.math) {
        return format!("#text(\"{}\")", escape_typst_string(value));
    }
    let mut content = String::from("[");
    for span in spans {
        if span.text.is_empty() {
            continue;
        }
        let escaped = escape_typst_string(&span.text);
        if span.math {
            content.push_str(&format!("#cmarker.render(\"{escaped}\", math: mitex)"));
        } else {
            content.push_str(&format!("#text(\"{escaped}\")"));
        }
    }
    content.push(']');
    content
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TypstTextSpan {
    text: String,
    math: bool,
}

fn recognized_math_spans(value: &str) -> Vec<TypstTextSpan> {
    let mut spans = Vec::new();
    let mut cursor = 0usize;
    while let Some((start, end, normalized)) = next_math_span(value, cursor) {
        if start > cursor {
            spans.push(TypstTextSpan {
                text: value[cursor..start].to_string(),
                math: false,
            });
        }
        spans.push(TypstTextSpan {
            text: normalized,
            math: true,
        });
        cursor = end;
    }
    if cursor < value.len() {
        spans.push(TypstTextSpan {
            text: value[cursor..].to_string(),
            math: false,
        });
    }
    if spans.is_empty() {
        spans.push(TypstTextSpan {
            text: value.to_string(),
            math: false,
        });
    }
    spans
}

fn next_math_span(value: &str, search_from: usize) -> Option<(usize, usize, String)> {
    let bytes = value.as_bytes();
    let mut index = search_from;
    while index < bytes.len() {
        if bytes[index] == b'$' && !is_escaped(bytes, index) {
            let delimiter_len = usize::from(bytes.get(index + 1) == Some(&b'$')) + 1;
            let content_start = index + delimiter_len;
            let mut cursor = content_start;
            while cursor + delimiter_len <= bytes.len() {
                if bytes[cursor] == b'$'
                    && !is_escaped(bytes, cursor)
                    && (delimiter_len == 1 || bytes.get(cursor + 1) == Some(&b'$'))
                {
                    let content = value[content_start..cursor].trim();
                    if !content.is_empty() {
                        let delimiter = "$".repeat(delimiter_len);
                        return Some((
                            index,
                            cursor + delimiter_len,
                            format!("{delimiter}{content}{delimiter}"),
                        ));
                    }
                    break;
                }
                cursor += value[cursor..].chars().next()?.len_utf8();
            }
        } else if bytes[index] == b'\\'
            && !is_escaped(bytes, index)
            && matches!(bytes.get(index + 1), Some(b'(' | b'['))
        {
            let opening = bytes[index + 1];
            let closing = if opening == b'(' { b')' } else { b']' };
            let content_start = index + 2;
            let mut cursor = content_start;
            while cursor + 1 < bytes.len() {
                if bytes[cursor] == b'\\'
                    && bytes[cursor + 1] == closing
                    && !is_escaped(bytes, cursor)
                {
                    let content = value[content_start..cursor].trim();
                    if !content.is_empty() {
                        let delimiter = if opening == b'(' { "$" } else { "$$" };
                        return Some((
                            index,
                            cursor + 2,
                            format!("{delimiter}{content}{delimiter}"),
                        ));
                    }
                    break;
                }
                cursor += value[cursor..].chars().next()?.len_utf8();
            }
        }
        index += value[index..].chars().next()?.len_utf8();
    }
    None
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
    let source = mupdf_path(source_pdf)
        .and_then(Document::open)
        .map_err(|error| {
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

#[cfg(test)]
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

fn validate_file_stem(file_stem: &str) -> Result<(), TypstError> {
    if matches!(file_stem, "translation-overlay" | "translated-document") {
        Ok(())
    } else {
        Err(TypstError::process(format!(
            "unsupported Typst worker source file stem {file_stem:?}"
        )))
    }
}

fn validate_compiler_assets(assets_dir: &Path, fonts_dir: &Path) -> Result<(), TypstError> {
    validate_asset(
        &fonts_dir.join("fonts"),
        "glyphst-fonts checkout (set GLYPHST_FONTS_DIR)",
    )?;
    validate_asset(
        &assets_dir.join("typst/packages/preview/cmarker/0.1.8"),
        "cmarker Typst package",
    )?;
    validate_asset(
        &assets_dir.join("typst/packages/preview/mitex/0.2.6"),
        "mitex Typst package",
    )
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

fn validate_file_asset(path: &Path, label: &str) -> Result<(), TypstError> {
    if path.is_file() {
        Ok(())
    } else {
        Err(TypstError::missing_asset(format!(
            "{label} is missing at {}",
            path.display()
        )))
    }
}

fn write_pdf_atomically(output_path: &Path, contents: &[u8]) -> Result<(), TypstError> {
    let part_path = output_path.with_extension("pdf.part");
    match fs::remove_file(&part_path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(TypstError::io(format!(
                "failed to remove stale Typst output {}: {error}",
                part_path.display()
            )));
        }
    }
    fs::write(&part_path, contents).map_err(|error| {
        TypstError::io(format!(
            "failed to write embedded Typst output {}: {error}",
            part_path.display()
        ))
    })?;

    match fs::rename(&part_path, output_path) {
        Ok(()) => Ok(()),
        Err(error)
            if output_path.is_file()
                && matches!(
                    error.kind(),
                    io::ErrorKind::AlreadyExists | io::ErrorKind::PermissionDenied
                ) =>
        {
            fs::remove_file(output_path).map_err(|remove_error| {
                TypstError::io(format!(
                    "failed to replace prior Typst output {}: {remove_error}",
                    output_path.display()
                ))
            })?;
            fs::rename(&part_path, output_path).map_err(|rename_error| {
                TypstError::io(format!(
                    "failed to publish embedded Typst output {}: {rename_error}",
                    output_path.display()
                ))
            })
        }
        Err(error) => Err(TypstError::io(format!(
            "failed to publish embedded Typst output {}: {error}",
            output_path.display()
        ))),
    }
}

fn format_typst_diagnostics(world: &GlyphstTypstWorld, diagnostics: &[SourceDiagnostic]) -> String {
    let mut output = NoColor::new(Vec::new());
    let emitted = emit(
        &mut output,
        world,
        diagnostics.iter(),
        DiagnosticFormat::Human,
    )
    .is_ok();
    let rendered = if emitted {
        String::from_utf8_lossy(&output.into_inner()).into_owned()
    } else {
        diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let rendered = rendered.trim();
    if rendered.is_empty() {
        "Typst compilation failed without diagnostics".to_string()
    } else {
        rendered.chars().take(8_000).collect()
    }
}

fn spawn_pipe_reader<R>(pipe: Option<R>) -> JoinHandle<String>
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || read_pipe_bounded(pipe, TYPST_WORKER_OUTPUT_LIMIT))
}

fn join_pipe_reader(reader: JoinHandle<String>, stream_name: &str) -> Result<String, TypstError> {
    reader.join().map_err(|_| {
        TypstError::process(format!(
            "failed to collect Typst worker {stream_name} because its reader panicked"
        ))
    })
}

fn read_pipe_bounded(pipe: Option<impl Read>, limit: usize) -> String {
    let Some(mut pipe) = pipe else {
        return String::new();
    };
    let mut output = Vec::with_capacity(limit.min(8 * 1024));
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = match pipe.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let remaining = limit.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..read.min(remaining)]);
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn worker_failure_message(summary: String, stdout: &str, stderr: &str) -> String {
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    if detail.is_empty() {
        summary
    } else {
        format!("{summary}: {detail}").chars().take(8_000).collect()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::{
        BoundingBox, FontWeight, InlineObject, InlineObjectPart, LayoutItem, LayoutPage,
        PageGeometry, TableOfContentsLayout, TableOfContentsLeader, inline_object_marker,
        layout::LayoutDocument,
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
                    structure_group_id: None,
                    toc: None,
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
                                baseline_position: BaselinePosition::Superscript,
                                ..math_style.clone()
                            }),
                        },
                        StyledRun {
                            text: "+n2d)".to_string(),
                            style: Some(math_style.clone()),
                        },
                        StyledRun {
                            text: " + ".to_string(),
                            style: Some(TextStyle {
                                font_class: Some(FontFamilyClass::Serif),
                                font_size: Some(10.0),
                                ..TextStyle::default()
                            }),
                        },
                        StyledRun {
                            text: "α".to_string(),
                            style: Some(math_style.clone()),
                        },
                        StyledRun {
                            text: "recency".to_string(),
                            style: Some(TextStyle {
                                font_size: Some(8.0),
                                baseline_position: BaselinePosition::Subscript,
                                ..math_style.clone()
                            }),
                        },
                        StyledRun {
                            text: "·".to_string(),
                            style: Some(TextStyle {
                                font_class: Some(FontFamilyClass::Serif),
                                font_size: Some(10.0),
                                ..TextStyle::default()
                            }),
                        },
                        StyledRun {
                            text: "recency".to_string(),
                            style: Some(math_style.clone()),
                        },
                        StyledRun {
                            text: "+".to_string(),
                            style: Some(TextStyle {
                                font_class: Some(FontFamilyClass::Serif),
                                font_size: Some(10.0),
                                ..TextStyle::default()
                            }),
                        },
                        StyledRun {
                            text: "importance".to_string(),
                            style: Some(math_style),
                        },
                    ],
                    inline_objects: Vec::new(),
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
        assert!(source.contains("#text(\"+ \")"));
        assert!(!source.contains("cmarker.render(\" + \")"));
        assert!(!source.contains("cmarker.render(\"·\")"));
        assert!(source.contains('\u{200B}'));
        assert!(source.contains("#super[#text(\"3\")]"));
        assert!(source.contains("#sub[#text(\"recency\")]"));
        assert!(!source.contains("#text(\"O(n\")\n#super"));
        assert!(!source.contains("clip: true"));
        assert!(source.contains("let size-floor = if strict"));
        assert!(source.contains("else { 0em }"));
    }

    #[test]
    fn semantic_newlines_use_explicit_typst_linebreaks() {
        let item = LayoutItem {
            unit_id: "dialogue".to_string(),
            block_id: "dialogue-block".to_string(),
            page_index: 0,
            source_bbox: BoundingBox::new(10.0, 10.0, 190.0, 100.0),
            source_regions: Vec::new(),
            structure_group_id: None,
            toc: None,
            bbox: BoundingBox::new(10.0, 10.0, 190.0, 100.0),
            text: "甲：第一句。\n乙：第二句。".to_string(),
            role: crate::BlockRole::Dialogue,
            source_font_size_pt: 10.0,
            font_size_pt: 10.0,
            minimum_font_size_pt: 7.0,
            emergency_minimum_font_size_pt: 5.0,
            leading_em: 0.58,
            minimum_leading_em: 0.4,
            font_weight: FontWeight::Regular,
            font_class: FontFamilyClass::Serif,
            italic: false,
            underline: false,
            strikeout: false,
            color_argb: 0xff00_0000,
            styled_runs: Vec::new(),
            inline_objects: Vec::new(),
            first_line_indent_pt: 0.0,
            justify: false,
            preserve_line_breaks: true,
        };
        let layout = LayoutDocument {
            pages: vec![LayoutPage {
                page_index: 0,
                geometry: PageGeometry::new(200.0, 200.0),
                items: vec![item],
            }],
            warnings: Vec::new(),
            preserved_on_overflow: Vec::new(),
            overflow_policy: OverflowPolicy::Adaptive,
        };

        let source = generate_typst_source(&layout);

        assert!(source.contains("第一句。"));
        assert!(source.contains("#linebreak()"));
        assert!(!source.contains("  \n"));
    }

    #[test]
    fn ordinary_prose_metacharacters_are_literal_while_formula_spans_use_cmarker() {
        let mut item = test_layout_item("* 表示项目负责人；†表示技术总监。公式 $x + y$。");
        item.block_id = "literal-marker".to_string();
        let source = generate_typst_source(&test_layout(item));

        assert!(source.contains("#text(\"* 表示项目负责人；†表示技术总监。公式 \")"));
        assert!(source.contains("#cmarker.render(\"$x + y$\", math: mitex)"));
        assert!(!source.contains("#cmarker.render(\"* 表示项目负责人"));
    }

    #[test]
    fn toc_rows_render_source_owned_leaders_and_page_labels() {
        let mut item = test_layout_item("流水线框架");
        item.role = crate::BlockRole::TableOfContents;
        item.toc = Some(TableOfContentsLayout {
            marker: Some("2.1".to_string()),
            page_label: "4".to_string(),
            leader: Some(TableOfContentsLeader {
                glyph: ".".to_string(),
                gap_em: 0.32,
            }),
        });
        let source = generate_typst_source(&test_layout(item));

        assert!(source.contains("#text(\"2.1\")"));
        assert!(source.contains("#text(\"流水线框架\")"));
        assert!(source.contains("#box(width: 1fr, repeat(text(\".\"), gap: 0.320em))"));
        assert!(source.contains("#text(\"4\")"));
    }

    #[test]
    fn inline_objects_anchor_their_pdf_baseline_from_the_box_bottom() {
        let marker = inline_object_marker("icon-1");
        let mut item = test_layout_item(&format!("before{marker}after"));
        item.inline_objects.push(InlineObject {
            id: "icon-1".to_string(),
            page_index: 0,
            bbox: BoundingBox::new(20.0, 10.0, 32.0, 20.0),
            baseline_y: 20.0,
            parts: vec![InlineObjectPart {
                source_block_index: 1,
                bbox: BoundingBox::new(20.0, 10.0, 32.0, 20.0),
            }],
        });

        let source = generate_typst_source(&test_layout(item.clone()));
        assert!(source.contains("baseline: (at: bottom, shift: text-size * 0.00000)"));
        assert!(!source.contains("baseline: 100"));

        item.inline_objects[0].baseline_y = 18.0;
        let raised = generate_typst_source(&test_layout(item.clone()));
        assert!(raised.contains("baseline: (at: bottom, shift: text-size * 0.20000)"));

        item.inline_objects[0].baseline_y = 22.0;
        let lowered = generate_typst_source(&test_layout(item));
        assert!(lowered.contains("baseline: (at: bottom, shift: text-size * -0.20000)"));
    }

    fn test_layout(item: LayoutItem) -> LayoutDocument {
        LayoutDocument {
            pages: vec![LayoutPage {
                page_index: 0,
                geometry: PageGeometry::new(200.0, 200.0),
                items: vec![item],
            }],
            warnings: Vec::new(),
            preserved_on_overflow: Vec::new(),
            overflow_policy: OverflowPolicy::Adaptive,
        }
    }

    fn test_layout_item(text: &str) -> LayoutItem {
        LayoutItem {
            unit_id: "unit".to_string(),
            block_id: "block".to_string(),
            page_index: 0,
            source_bbox: BoundingBox::new(10.0, 10.0, 190.0, 30.0),
            source_regions: Vec::new(),
            structure_group_id: None,
            toc: None,
            bbox: BoundingBox::new(10.0, 10.0, 190.0, 30.0),
            text: text.to_string(),
            role: crate::BlockRole::Body,
            source_font_size_pt: 10.0,
            font_size_pt: 10.0,
            minimum_font_size_pt: 7.0,
            emergency_minimum_font_size_pt: 5.0,
            leading_em: 0.58,
            minimum_leading_em: 0.4,
            font_weight: FontWeight::Regular,
            font_class: FontFamilyClass::Serif,
            italic: false,
            underline: false,
            strikeout: false,
            color_argb: 0xff00_0000,
            styled_runs: Vec::new(),
            inline_objects: Vec::new(),
            first_line_indent_pt: 0.0,
            justify: false,
            preserve_line_breaks: false,
        }
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
    fn rejects_unknown_worker_file_stems() {
        let error = validate_file_stem("../document").unwrap_err();

        assert_eq!(error.kind, TypstErrorKind::Process);
        assert!(error.message.contains("unsupported"));
    }

    #[test]
    fn restricted_loader_denies_other_project_files_and_packages() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("glyphst-typst-loader-test-{nonce:x}"));
        fs::create_dir_all(&directory).unwrap();
        let loader = RestrictedFileLoader::new(
            &directory,
            &resolve_translation_assets_dir(),
            "translation-overlay.typ".to_string(),
        )
        .unwrap();

        let project_file = RootedPath::new(
            VirtualRoot::Project,
            VirtualPath::new("unrelated.typ").unwrap(),
        )
        .intern();
        assert!(matches!(
            loader.load(project_file),
            Err(FileError::AccessDenied)
        ));

        let package = "@preview/example:1.0.0"
            .parse::<typst::syntax::package::PackageSpec>()
            .unwrap();
        let package_file = RootedPath::new(
            VirtualRoot::Package(package),
            VirtualPath::new("lib.typ").unwrap(),
        )
        .intern();
        assert!(matches!(
            loader.load(package_file),
            Err(FileError::Other(Some(_)))
        ));

        let _ = fs::remove_dir_all(directory);
    }

    #[cfg(unix)]
    #[test]
    fn restricted_root_denies_symlinks_outside_its_root() {
        use std::os::unix::fs::symlink;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("glyphst-typst-symlink-test-{nonce:x}"));
        let job_directory = directory.join("job");
        fs::create_dir_all(&job_directory).unwrap();
        let outside_file = directory.join("outside.pdf");
        fs::write(&outside_file, b"not a PDF").unwrap();
        symlink(&outside_file, job_directory.join("source.pdf")).unwrap();
        let root = RestrictedRoot::new(&job_directory, "test root").unwrap();

        let result = root.load(&VirtualPath::new("source.pdf").unwrap());

        assert!(matches!(result, Err(FileError::AccessDenied)));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn crowded_translations_shrink_before_their_lines_collide() {
        // Three lines' worth of text in a one-line box, adaptive: the text
        // goes small, its lines stay apart.
        let mut layout = LayoutDocument {
            pages: vec![LayoutPage {
                page_index: 0,
                geometry: PageGeometry::new(240.0, 120.0),
                items: Vec::new(),
            }],
            warnings: Vec::new(),
            preserved_on_overflow: Vec::new(),
            overflow_policy: OverflowPolicy::Adaptive,
        };
        let bounds = BoundingBox::new(10.0, 10.0, 160.0, 24.0);
        layout.pages[0].items.push(LayoutItem {
            unit_id: "unit".to_string(),
            block_id: "block".to_string(),
            page_index: 0,
            source_bbox: bounds,
            source_regions: vec![bounds],
            structure_group_id: None,
            toc: None,
            bbox: bounds,
            text: "Les pompes à chaleur tirent leur énergie de l'air ambiant, de façon \
                   écologique, et la transforment en chaleur agréable pour la maison."
                .to_string(),
            role: crate::BlockRole::Body,
            source_font_size_pt: 10.0,
            font_size_pt: 10.0,
            minimum_font_size_pt: 7.0,
            emergency_minimum_font_size_pt: 5.5,
            leading_em: 0.58,
            minimum_leading_em: 0.45,
            font_weight: FontWeight::Regular,
            font_class: FontFamilyClass::Serif,
            italic: false,
            underline: false,
            strikeout: false,
            color_argb: 0xff00_0000,
            styled_runs: Vec::new(),
            inline_objects: Vec::new(),
            first_line_indent_pt: 0.0,
            justify: false,
            preserve_line_breaks: false,
        });
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("glyphst-typst-crowded-{nonce:x}"));
        let output = TypstCompiler::new(TypstCompilerConfig::default())
            .compile(&layout, &directory, &CancellationToken::new())
            .unwrap();
        let document = mupdf::Document::open(output.pdf_path.to_str().unwrap()).unwrap();
        let page = document.load_page(0).unwrap();
        let text = page.to_text_page(mupdf::TextPageFlags::empty()).unwrap();
        // Each line's top and its glyphs' height.
        let mut lines: Vec<(f32, f32)> = Vec::new();
        for block in text.blocks() {
            for line in block.lines() {
                let bounds = line.bounds();
                lines.push((bounds.y0, bounds.y1 - bounds.y0));
            }
        }
        assert!(lines.len() >= 2, "{lines:?}");
        for pair in lines.windows(2) {
            let ((top, height), (next, _)) = (pair[0], pair[1]);
            assert!(
                next - top >= 0.8 * height,
                "lines {top} and {next} ({height} tall) run into each other: {lines:?}"
            );
        }
        let _ = fs::remove_dir_all(directory);
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
                    structure_group_id: None,
                    toc: None,
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
                    inline_objects: Vec::new(),
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
        assert!(!output.pdf_path.with_extension("pdf.part").exists());
        let _ = fs::remove_dir_all(directory);
    }
}

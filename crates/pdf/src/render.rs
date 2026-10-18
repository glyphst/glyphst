use std::{
    ops::Deref,
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
};

use anyhow::{Context as _, Result, anyhow};
use glyphst_core::DocumentLayout;
use mupdf::{Device, DisplayList, Matrix, Rect};

use crate::DocumentSnapshot;

enum RenderRequest {
    CreateDisplayList {
        page_idx: usize,
        page_bounds: Rect,
        reply: SyncSender<Result<DisplayList>>,
    },
}

struct DocumentRenderSessionInner {
    requests: Option<mpsc::Sender<RenderRequest>>,
    worker: Option<JoinHandle<()>>,
}

impl Drop for DocumentRenderSessionInner {
    fn drop(&mut self) {
        self.requests.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Clone)]
pub struct DocumentRenderSession {
    inner: Arc<DocumentRenderSessionInner>,
}

impl DocumentRenderSession {
    pub fn open(snapshot: Arc<DocumentSnapshot>, document_layout: DocumentLayout) -> Result<Self> {
        let (request_tx, request_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("glyphst-pdf-render".to_string())
            .spawn(move || {
                Self::run_worker(snapshot, document_layout, request_rx, ready_tx);
            })
            .context("failed to start PDF render session")?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                inner: Arc::new(DocumentRenderSessionInner {
                    requests: Some(request_tx),
                    worker: Some(worker),
                }),
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                let _ = worker.join();
                Err(anyhow!("PDF render session stopped during initialization"))
            }
        }
    }

    pub fn create_display_list(
        &self,
        page_idx: usize,
        page_bounds: Rect,
    ) -> Result<RetainedDisplayList> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let request = RenderRequest::CreateDisplayList {
            page_idx,
            page_bounds,
            reply: reply_tx,
        };
        self.inner
            .requests
            .as_ref()
            .ok_or_else(|| anyhow!("PDF render session is closed"))?
            .send(request)
            .map_err(|_| anyhow!("PDF render session stopped before accepting a request"))?;
        let display_list = reply_rx
            .recv()
            .map_err(|_| anyhow!("PDF render session stopped before completing a request"))??;
        Ok(RetainedDisplayList::new(display_list, self.clone()))
    }

    fn run_worker(
        snapshot: Arc<DocumentSnapshot>,
        document_layout: DocumentLayout,
        requests: Receiver<RenderRequest>,
        ready: SyncSender<Result<()>>,
    ) {
        let document = match snapshot
            .open_with_layout(document_layout)
            .context("failed to open document for rendering")
        {
            Ok(document) => document,
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        };
        if ready.send(Ok(())).is_err() {
            return;
        }

        while let Ok(request) = requests.recv() {
            match request {
                RenderRequest::CreateDisplayList {
                    page_idx,
                    page_bounds,
                    reply,
                } => {
                    let result = (|| {
                        let page = document
                            .inner()
                            .load_page(page_idx as i32)
                            .with_context(|| format!("failed to load PDF page {}", page_idx + 1))?;
                        let mut display_list =
                            DisplayList::new(page_bounds).with_context(|| {
                                format!(
                                    "failed to create display list for PDF page {}",
                                    page_idx + 1
                                )
                            })?;
                        let device =
                            Device::from_display_list(&mut display_list).with_context(|| {
                                format!(
                                    "failed to create display-list device for PDF page {}",
                                    page_idx + 1
                                )
                            })?;
                        page.run(&device, &Matrix::IDENTITY).with_context(|| {
                            format!("failed to record PDF page {}", page_idx + 1)
                        })?;
                        drop(device);
                        Ok(display_list)
                    })();
                    let _ = reply.send(result);
                }
            }
        }
    }
}

struct RetainedDisplayListInner {
    // Field order is intentional: MuPDF must drop the list before the session
    // releases the document resources referenced by Type 3 fonts.
    display_list: DisplayList,
    _session: DocumentRenderSession,
}

#[derive(Clone)]
pub struct RetainedDisplayList {
    inner: Arc<RetainedDisplayListInner>,
}

impl RetainedDisplayList {
    fn new(display_list: DisplayList, session: DocumentRenderSession) -> Self {
        Self {
            inner: Arc::new(RetainedDisplayListInner {
                display_list,
                _session: session,
            }),
        }
    }
}

impl AsRef<DisplayList> for RetainedDisplayList {
    fn as_ref(&self) -> &DisplayList {
        &self.inner.display_list
    }
}

impl Deref for RetainedDisplayList {
    type Target = DisplayList;

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use glyphst_core::DocumentLayout;
    use mupdf::{Colorspace, Device, Matrix, Pixmap, Rect};

    use super::DocumentRenderSession;
    use crate::{DocumentPassword, DocumentSnapshot};

    fn encrypted_pdf_bytes() -> Vec<u8> {
        let encoded = include_str!("../tests/fixtures/dummy-encrypted.pdf.b64")
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        STANDARD.decode(encoded).unwrap()
    }

    struct TempPdf(PathBuf);

    impl TempPdf {
        fn new(bytes: &[u8]) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "glyphst-type3-render-{}-{nonce}.pdf",
                std::process::id()
            ));
            fs::write(&path, bytes).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempPdf {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn append_object(pdf: &mut Vec<u8>, offsets: &mut Vec<usize>, id: usize, body: &[u8]) {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
    }

    fn type3_pdf() -> Vec<u8> {
        let page_content = b"0 0 1 rg\nBT\n/F1 40 Tf\n0 1 -1 0 70 20 Tm\n(A) Tj\nET\n";
        let glyph_content = b"1000 0 d0\n100 100 800 800 re\nf\n";
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();

        append_object(
            &mut pdf,
            &mut offsets,
            1,
            b"<< /Type /Catalog /Pages 2 0 R >>",
        );
        append_object(
            &mut pdf,
            &mut offsets,
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        );
        append_object(
            &mut pdf,
            &mut offsets,
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
              /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>",
        );
        append_object(
            &mut pdf,
            &mut offsets,
            4,
            b"<< /Type /Font /Subtype /Type3 /Name /F1 \
              /FontBBox [0 0 1000 1000] /FontMatrix [.001 0 0 .001 0 0] \
              /CharProcs << /A 6 0 R >> \
              /Encoding << /Type /Encoding /Differences [65 /A] >> \
              /FirstChar 65 /LastChar 65 /Widths [1000] /Resources << >> >>",
        );
        append_object(
            &mut pdf,
            &mut offsets,
            5,
            format!(
                "<< /Length {} >>\nstream\n{}endstream",
                page_content.len(),
                String::from_utf8_lossy(page_content)
            )
            .as_bytes(),
        );
        append_object(
            &mut pdf,
            &mut offsets,
            6,
            format!(
                "<< /Length {} >>\nstream\n{}endstream",
                glyph_content.len(),
                String::from_utf8_lossy(glyph_content)
            )
            .as_bytes(),
        );

        let xref_offset = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    fn test_session() -> (TempPdf, Arc<DocumentSnapshot>, DocumentRenderSession) {
        let fixture = TempPdf::new(&type3_pdf());
        let snapshot = Arc::new(DocumentSnapshot::read(fixture.path()).unwrap());
        let session = DocumentRenderSession::open(snapshot.clone(), DocumentLayout::Fixed).unwrap();
        (fixture, snapshot, session)
    }

    #[test]
    fn render_session_reopens_encrypted_snapshot_with_its_password() {
        let snapshot = Arc::new(
            DocumentSnapshot::from_pdf_bytes("encrypted.pdf", encrypted_pdf_bytes())
                .with_password(DocumentPassword::new("123456")),
        );
        let session = DocumentRenderSession::open(snapshot, DocumentLayout::Fixed).unwrap();
        let display_list = session
            .create_display_list(0, Rect::new(0.0, 0.0, 595.0, 842.0))
            .unwrap();
        assert!(!display_list.is_empty());
    }

    #[test]
    fn retained_type3_list_renders_after_explicit_session_is_dropped() {
        let (_fixture, snapshot, session) = test_session();
        let display_list = session
            .create_display_list(0, Rect::new(0.0, 0.0, 100.0, 100.0))
            .unwrap();

        let replacement = DocumentRenderSession::open(snapshot, DocumentLayout::Fixed).unwrap();
        drop(session);

        let has_ink = std::thread::spawn(move || {
            let mut pixmap =
                Pixmap::new_with_w_h(&Colorspace::device_rgb(), 100, 100, true).unwrap();
            pixmap.samples_mut().fill(255);
            let device = Device::from_pixmap(&pixmap).unwrap();
            display_list
                .run(&device, &Matrix::IDENTITY, Rect::INF)
                .unwrap();
            pixmap
                .samples()
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[0] < 240 || pixel[1] < 240 || pixel[2] < 240)
        })
        .join()
        .unwrap();

        assert!(has_ink, "the retained Type 3 glyph should be visible");
        drop(replacement);
    }

    #[test]
    fn display_list_request_errors_do_not_stop_the_session() {
        let (_fixture, _snapshot, session) = test_session();
        let bounds = Rect::new(0.0, 0.0, 100.0, 100.0);

        let error = match session.create_display_list(1, bounds) {
            Ok(_) => panic!("out-of-bounds page unexpectedly rendered"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("failed to load PDF page 2"));
        assert!(session.create_display_list(0, bounds).is_ok());
    }
}

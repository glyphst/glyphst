use std::path::{Path, PathBuf};

use glyphst_core::DocumentFormat;
use glyphst_settings::ui_rems;
use glyphst_ui::icons::{Icon, IconName};
use gpui::{AnyElement, ElementId, ExternalPaths, IntoElement, div, prelude::*, px};
use gpui_component::ThemeColor;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ExternalDocumentDrop {
    pub(crate) supported: Vec<PathBuf>,
    pub(crate) rejected: Vec<PathBuf>,
}

impl ExternalDocumentDrop {
    pub(crate) fn feedback(&self) -> Option<ExternalDocumentDropFeedback> {
        let supported = self.supported.len();
        let rejected = self.rejected.len();
        (supported > 0 || rejected > 0).then_some(ExternalDocumentDropFeedback {
            supported,
            rejected,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExternalDocumentDropFeedback {
    pub(crate) supported: usize,
    pub(crate) rejected: usize,
}

pub(crate) fn is_supported_document_path(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|extension| extension.to_str())
            .and_then(DocumentFormat::from_extension)
            .is_some()
}

pub(crate) fn classify_external_document_paths(paths: &[PathBuf]) -> ExternalDocumentDrop {
    let mut drop = ExternalDocumentDrop::default();
    for path in paths {
        if is_supported_document_path(path) {
            drop.supported.push(path.clone());
        } else {
            drop.rejected.push(path.clone());
        }
    }
    drop
}

pub(crate) fn render_external_document_drop_overlay(
    feedback: ExternalDocumentDropFeedback,
    theme: ThemeColor,
) -> AnyElement {
    render_external_document_drop_overlay_with_identity(
        feedback,
        theme,
        "external-document-drop-overlay",
        "external-document-drop-overlay",
    )
}

pub(crate) fn render_external_document_drop_overlay_for_pane(
    feedback: ExternalDocumentDropFeedback,
    theme: ThemeColor,
    pane_id: u64,
) -> AnyElement {
    render_external_document_drop_overlay_with_identity(
        feedback,
        theme,
        ("pane-external-document-drop-overlay", pane_id),
        format!("pane-external-document-drop-overlay-{pane_id}"),
    )
}

fn render_external_document_drop_overlay_with_identity(
    feedback: ExternalDocumentDropFeedback,
    theme: ThemeColor,
    id: impl Into<ElementId>,
    selector_prefix: impl Into<String>,
) -> AnyElement {
    let accepts_documents = feedback.supported > 0;
    let accent = if accepts_documents {
        theme.drag_border
    } else {
        theme.danger
    };
    let background = if accepts_documents {
        theme.drop_target
    } else {
        theme.danger.opacity(0.12)
    };
    let title = if accepts_documents {
        crate::fl!(
            "external-drop-open-title",
            count = feedback.supported as i64
        )
    } else {
        crate::fl!("external-drop-unsupported-title")
    };
    let hint = if !accepts_documents {
        crate::fl!("external-drop-unsupported-hint")
    } else if feedback.rejected > 0 {
        crate::fl!("external-drop-mixed-hint", count = feedback.rejected as i64)
    } else {
        crate::fl!("external-drop-supported-hint")
    };
    let status = if accepts_documents {
        "accepted"
    } else {
        "rejected"
    };
    let selector = format!("{}-{status}", selector_prefix.into());

    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .absolute()
        .top_0()
        .right_0()
        .bottom_0()
        .left_0()
        .invisible()
        .drag_over::<ExternalPaths>(|style, _, _, _| style.visible())
        .flex()
        .items_center()
        .justify_center()
        .p_6()
        .bg(background)
        .border_2()
        .border_dashed()
        .border_color(accent)
        .child(
            div()
                .max_w(px(460.0))
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .rounded(px(12.0))
                .border_1()
                .border_color(accent.opacity(0.55))
                .bg(theme.background.opacity(0.94))
                .px_8()
                .py_6()
                .shadow_lg()
                .child(
                    Icon::new(if accepts_documents {
                        IconName::FileText
                    } else {
                        IconName::TriangleAlert
                    })
                    .size(ui_rems(32.0))
                    .text_color(accent),
                )
                .child(
                    div()
                        .text_center()
                        .text_size(ui_rems(16.0))
                        .text_color(theme.foreground)
                        .child(title),
                )
                .child(
                    div()
                        .text_center()
                        .text_size(ui_rems(12.0))
                        .text_color(theme.muted_foreground)
                        .child(hint),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::{fs, time::SystemTime};

    use super::*;

    fn temporary_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "glyphst-external-drop-{}-{}-{name}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn classifies_supported_documents_and_rejects_other_items_in_order() {
        let pdf = temporary_path("paper.PDF");
        let epub = temporary_path("book.ePuB");
        let image = temporary_path("cover.png");
        let folder = temporary_path("folder.pdf");
        fs::write(&pdf, b"pdf").unwrap();
        fs::write(&epub, b"epub").unwrap();
        fs::write(&image, b"png").unwrap();
        fs::create_dir(&folder).unwrap();

        let missing = temporary_path("missing.pdf");
        let classified = classify_external_document_paths(&[
            pdf.clone(),
            image.clone(),
            epub.clone(),
            folder.clone(),
            missing.clone(),
        ]);

        assert_eq!(classified.supported, vec![pdf.clone(), epub.clone()]);
        assert_eq!(
            classified.rejected,
            vec![image.clone(), folder.clone(), missing]
        );
        assert_eq!(
            classified.feedback(),
            Some(ExternalDocumentDropFeedback {
                supported: 2,
                rejected: 3,
            })
        );

        fs::remove_file(pdf).unwrap();
        fs::remove_file(epub).unwrap();
        fs::remove_file(image).unwrap();
        fs::remove_dir(folder).unwrap();
    }
}

pub mod ai_chat;
pub mod ai_client;

pub use glyphst_settings as settings;

pub mod context {
    use glyphst_core::{ImageArtifact, TextSelection};

    #[derive(Clone, Debug, PartialEq)]
    pub enum Attachment {
        Text(String),
        Image(ImageArtifact),
        Selection(TextSelection),
    }
}

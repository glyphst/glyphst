//! Setting meaning search up: llama.cpp's server (unless one is given or
//! already there), then a model, through the models crate's setup.

use std::path::PathBuf;

pub use glyphst_models::setup::*;

use crate::manifest::{self, Asset, MODEL};

/// Meaning search's: what `root` holds now; `custom_engine` replaces the
/// download.
pub fn meaning(root: PathBuf, custom_engine: Option<PathBuf>) -> Setup {
    for_model(root, custom_engine, MODEL)
}

/// Another model on the same engine (the reranker): the engine is fetched
/// only when it is not there yet.
pub fn for_model(root: PathBuf, custom_engine: Option<PathBuf>, model: Asset) -> Setup {
    let engine = if custom_engine.is_some() {
        None
    } else {
        manifest::engine()
    };
    Setup::with(root, engine, custom_engine, vec![model])
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::install;

    #[test]
    fn a_setup_offers_what_it_kept_and_is_ready_with_its_files() {
        let root = std::env::temp_dir().join(format!("glyphst-embed-setup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // The user's own server, and no model yet.
        let exe = root.join("my-llama-server");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&exe, b"").unwrap();
        let setup = meaning(root.clone(), Some(exe.clone()));
        assert_eq!(
            setup.state(),
            SetupState::Offer {
                kept: 0,
                total: MODEL.size
            },
            "only the model to fetch"
        );
        assert_eq!(setup.paths(), None);

        // A model on disk: ready.
        let model = install::model_path(&root, &MODEL);
        std::fs::create_dir_all(model.parent().unwrap()).unwrap();
        let file = std::fs::File::create(&model).unwrap();
        file.set_len(MODEL.size).unwrap();
        let setup = meaning(root.clone(), Some(exe.clone()));
        assert!(setup.is_ready());
        assert_eq!(setup.paths(), Some((exe.clone(), model)));

        // The reranker on the same engine: only its own model to fetch.
        let reranker = for_model(root.clone(), Some(exe), manifest::RERANKER);
        assert_eq!(
            reranker.state(),
            SetupState::Offer {
                kept: 0,
                total: manifest::RERANKER.size
            }
        );

        // Meaning search downloaded the engine: the reranker shares it.
        if let Some(engine) = manifest::engine() {
            let before = for_model(root.clone(), None, manifest::RERANKER);
            assert_eq!(
                before.state(),
                SetupState::Offer {
                    kept: 0,
                    total: engine.archive.size + manifest::RERANKER.size
                }
            );
            let exe = install::engine_dir(&root, &engine).join(engine.exe);
            std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
            std::fs::write(&exe, b"").unwrap();
            let after = for_model(root.clone(), None, manifest::RERANKER);
            assert_eq!(
                after.state(),
                SetupState::Offer {
                    kept: 0,
                    total: manifest::RERANKER.size
                }
            );
            assert!(meaning(root.clone(), None).is_ready());
        }
        let _ = std::fs::remove_dir_all(root);
    }
}

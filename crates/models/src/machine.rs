//! What this computer has, for suggesting the model that suits it: the
//! default balances download, speed and accuracy even on a fast machine,
//! and a bigger one is only suggested, never downloaded unasked.

use crate::catalog::{self, Task};

/// The computer's processors, memory and graphics.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Machine {
    pub cores: usize,
    /// Bytes of memory, when the system says.
    pub memory: Option<u64>,
    pub gpu: Option<Gpu>,
}

/// The graphics adapter the app draws with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Gpu {
    pub name: String,
    /// A card of its own rather than part of the processor.
    pub discrete: bool,
    pub nvidia: bool,
    /// Bytes of its own memory, when known (llama.cpp reports it).
    pub memory: Option<u64>,
}

const GIB: u64 = 1 << 30;

impl Machine {
    /// This computer, with the `gpu` the app found.
    pub fn detect(gpu: Option<Gpu>) -> Self {
        Self {
            cores: std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get),
            memory: system_memory(),
            gpu,
        }
    }

    /// The model that suits this computer for `task`.
    pub fn suggestion(&self, task: Task) -> &'static catalog::ModelEntry {
        let id = match task {
            Task::TextRecognition if self.is_modest() => "pp-ocrv6-tiny",
            Task::TextRecognition if self.gpu.as_ref().is_some_and(|gpu| gpu.discrete) => {
                "pp-ocrv6-medium"
            }
            Task::Embedding
                if self
                    .gpu
                    .as_ref()
                    .and_then(|gpu| gpu.memory)
                    .is_some_and(|memory| memory >= 8 * GIB) =>
            {
                "qwen3-embedding-4b"
            }
            _ => catalog::default_for(task).id,
        };
        catalog::entry(id).unwrap_or_else(|| catalog::default_for(task))
    }

    /// Few cores or little memory: the smallest models.
    fn is_modest(&self) -> bool {
        self.cores <= 4 || self.memory.is_some_and(|memory| memory < 8 * GIB)
    }

    /// `16 cores · 32 GB · Intel Iris Xe`, for the Models page.
    pub fn summary(&self) -> String {
        let mut parts = vec![format!("{} cores", self.cores)];
        if let Some(memory) = self.memory {
            parts.push(format!("{} GB", (memory + GIB / 2) / GIB));
        }
        match &self.gpu {
            Some(gpu) if gpu.discrete => parts.push(format!("{} (its own card)", gpu.name)),
            Some(gpu) => parts.push(format!("{} (integrated)", gpu.name)),
            None => parts.push("no GPU found".to_owned()),
        }
        parts.join(" · ")
    }
}

/// The system's memory in bytes.
fn system_memory() -> Option<u64> {
    if cfg!(target_os = "linux") {
        let info = std::fs::read_to_string("/proc/meminfo").ok()?;
        let line = info.lines().find(|line| line.starts_with("MemTotal:"))?;
        let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kib * 1024)
    } else if cfg!(target_os = "macos") {
        let output = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()?;
        String::from_utf8_lossy(&output.stdout).trim().parse().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(cores: usize, memory: u64, gpu: Option<Gpu>) -> Machine {
        Machine {
            cores,
            memory: Some(memory * GIB),
            gpu,
        }
    }

    #[test]
    fn suggestions_follow_the_machine_and_default_to_the_balanced_models() {
        let laptop = machine(8, 16, None);
        assert_eq!(
            laptop.suggestion(Task::TextRecognition).id,
            "pp-ocrv6-small"
        );
        assert_eq!(laptop.suggestion(Task::Embedding).id, "embeddinggemma-2");
        assert_eq!(laptop.suggestion(Task::Rerank).id, "bge-reranker-v2-m3");

        let small = machine(4, 8, None);
        assert_eq!(small.suggestion(Task::TextRecognition).id, "pp-ocrv6-tiny");
        let low_memory = machine(16, 4, None);
        assert_eq!(
            low_memory.suggestion(Task::TextRecognition).id,
            "pp-ocrv6-tiny"
        );

        let card = Gpu {
            name: "NVIDIA GeForce RTX 4070".into(),
            discrete: true,
            nvidia: true,
            memory: Some(12 * GIB),
        };
        let desktop = machine(16, 32, Some(card));
        assert_eq!(
            desktop.suggestion(Task::TextRecognition).id,
            "pp-ocrv6-medium"
        );
        assert_eq!(desktop.suggestion(Task::Embedding).id, "qwen3-embedding-4b");
        assert_eq!(desktop.suggestion(Task::Rerank).id, "bge-reranker-v2-m3");
        assert!(desktop.summary().contains("RTX 4070 (its own card)"));
    }
}

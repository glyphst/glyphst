//! The models that can run on this computer, by task: what each costs to
//! download and gives, its license, and its pinned files. Their engines
//! (llama.cpp, ONNX Runtime) are pinned by the crates that run them.

use std::path::PathBuf;

use crate::Asset;

/// What a model is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Task {
    /// Reading scanned pages (OCR).
    TextRecognition,
    /// Search by meaning: passages and queries as vectors.
    Embedding,
    /// Rereading the best matches against the query.
    Rerank,
    /// Chinese, Japanese and Korean faces for translations into them.
    Fonts,
}

impl Task {
    pub const ALL: [Self; 4] = [
        Self::TextRecognition,
        Self::Embedding,
        Self::Rerank,
        Self::Fonts,
    ];

    /// The folder its models (and their engine) live in.
    pub fn root(self) -> PathBuf {
        let data = glyphst_core::paths::data_dir();
        match self {
            Self::TextRecognition => std::env::var_os("GLYPHST_OCR_DIR")
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| data.join("ocr")),
            Self::Embedding | Self::Rerank => data.join("meaning-search"),
            Self::Fonts => glyphst_core::paths::fonts_download_dir(),
        }
    }

    /// The folder its files go in: the root's `models/`, or for fonts the
    /// fonts folder itself (`fonts/` in it holds the faces).
    pub fn files_dir(self) -> PathBuf {
        match self {
            Self::Fonts => self.root(),
            _ => self.root().join("models"),
        }
    }
}

/// A model's license, for the notices (the Models page doesn't show it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct License {
    pub name: &'static str,
    pub url: &'static str,
}

const OFL: License = License {
    name: "OFL-1.1",
    url: "https://openfontlicense.org/open-font-license-official-text/",
};

const APACHE: License = License {
    name: "Apache-2.0",
    url: "https://www.apache.org/licenses/LICENSE-2.0",
};

/// A model the app can download and run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelEntry {
    /// Stable, kept in the settings.
    pub id: &'static str,
    pub task: Task,
    pub name: &'static str,
    /// What it is good at and what it costs, in a sentence.
    pub note: &'static str,
    pub files: &'static [Asset],
    pub license: License,
    /// Runs on ONNX Runtime, which can use a GPU when asked to; llama.cpp
    /// uses one by itself.
    pub device_choice: bool,
    /// The one used until another is chosen.
    pub default: bool,
}

impl ModelEntry {
    /// Bytes to download.
    pub fn size(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }
}

/// Every model, by task, smallest first within a task.
pub const CATALOG: &[ModelEntry] = &[
    ModelEntry {
        id: "pp-ocrv6-tiny",
        task: Task::TextRecognition,
        name: "pp-ocrv6 tiny",
        note: "Fastest, least accurate (PaddleOCR: detection 80.6, recognition 73.5). For slow machines.",
        files: &[PP_OCRV6_TINY_DET, PP_OCRV6_TINY_REC, PP_OCRV6_TINY_DICT],
        license: APACHE,
        device_choice: true,
        default: false,
    },
    ModelEntry {
        id: "pp-ocrv6-small",
        task: Task::TextRecognition,
        name: "pp-ocrv6 small",
        note: "Balanced: detection 84.1, recognition 81.3. About 2 s a page on a laptop's CPU.",
        files: &[PP_OCRV6_SMALL_DET, PP_OCRV6_SMALL_REC, PP_OCRV6_DICT],
        license: APACHE,
        device_choice: true,
        default: true,
    },
    ModelEntry {
        id: "pp-ocrv6-medium",
        task: Task::TextRecognition,
        name: "pp-ocrv6 medium",
        note: "The most accurate PP-OCR: detection 86.2, recognition 83.2. Best with a dedicated GPU.",
        files: &[PP_OCRV6_MEDIUM_DET, PP_OCRV6_MEDIUM_REC, PP_OCRV6_DICT],
        license: APACHE,
        device_choice: true,
        default: false,
    },
    ModelEntry {
        id: "indra-v1.1.0",
        task: Task::TextRecognition,
        name: "indra",
        note: "Lines, layout blocks in reading order and tables, in 80+ languages. Fast on the CPU.",
        files: INDRA,
        license: License {
            name: "modified OpenRAIL-M",
            url: "https://huggingface.co/datalab-to/indra-ocr/blob/bd02167fdf2005a19962cc15188f8496a2c87d3b/LICENSE",
        },
        device_choice: true,
        default: false,
    },
    ModelEntry {
        id: "paddleocr-vl-1.5",
        task: Task::TextRecognition,
        name: "paddleocr-vl 1.5",
        note: "Layout, formulas and tables like Paddle online, on this computer (0.9B, with PP-DocLayoutV3). Experimental and slow: about 1.5 min a page on a laptop's CPU.",
        files: PADDLEOCR_VL,
        license: APACHE,
        device_choice: false,
        default: false,
    },
    ModelEntry {
        id: "embeddinggemma-2",
        task: Task::Embedding,
        name: "embeddinggemma 2",
        note: "Multilingual, 256-dimension vectors.",
        files: &[EMBEDDINGGEMMA],
        license: License {
            name: "Gemma Terms of Use",
            url: "https://ai.google.dev/gemma/terms",
        },
        device_choice: false,
        default: true,
    },
    ModelEntry {
        id: "qwen3-embedding-0.6b",
        task: Task::Embedding,
        name: "qwen3-embedding 0.6b",
        note: "Stronger on long passages and code; twice the download.",
        files: &[QWEN3_EMBEDDING_SMALL],
        license: APACHE,
        device_choice: false,
        default: false,
    },
    ModelEntry {
        id: "qwen3-embedding-4b",
        task: Task::Embedding,
        name: "qwen3-embedding 4b",
        note: "The best results; quick only with 8 GB of GPU memory.",
        files: &[QWEN3_EMBEDDING_LARGE],
        license: APACHE,
        device_choice: false,
        default: false,
    },
    ModelEntry {
        id: "cjk-fonts-sc",
        task: Task::Fonts,
        name: "chinese (simplified)",
        note: "Noto Sans CJK and Source Han Serif in Chinese (Simplified) forms, for translations into Chinese (Simplified).",
        files: CJK_FONTS_SC,
        license: OFL,
        device_choice: false,
        default: true,
    },
    ModelEntry {
        id: "cjk-fonts-tc",
        task: Task::Fonts,
        name: "chinese (traditional)",
        note: "Noto Sans CJK and Source Han Serif in Chinese (Traditional) forms, for translations into Chinese (Traditional).",
        files: CJK_FONTS_TC,
        license: OFL,
        device_choice: false,
        default: false,
    },
    ModelEntry {
        id: "cjk-fonts-jp",
        task: Task::Fonts,
        name: "japanese",
        note: "Noto Sans CJK and Source Han Serif in Japanese forms, for translations into Japanese.",
        files: CJK_FONTS_JP,
        license: OFL,
        device_choice: false,
        default: false,
    },
    ModelEntry {
        id: "cjk-fonts-kr",
        task: Task::Fonts,
        name: "korean",
        note: "Noto Sans CJK and Source Han Serif in Korean forms, for translations into Korean.",
        files: CJK_FONTS_KR,
        license: OFL,
        device_choice: false,
        default: false,
    },
    ModelEntry {
        id: "bge-reranker-v2-m3",
        task: Task::Rerank,
        name: "bge-reranker v2 m3",
        note: "Multilingual; rereads 30 passages in under half a second on an integrated GPU.",
        files: &[BGE_RERANKER],
        license: APACHE,
        device_choice: false,
        default: true,
    },
    ModelEntry {
        id: "qwen3-reranker-0.6b",
        task: Task::Rerank,
        name: "qwen3-reranker 0.6b",
        note: "An alternative reranker; slower, and no better on the labelled set.",
        files: &[QWEN3_RERANKER],
        license: APACHE,
        device_choice: false,
        default: false,
    },
];

/// A Noto Sans CJK face (or the family's licence), from the commit of
/// notofonts/noto-cjk glyphst-fonts pins, in the pack's `fonts/<dir>/`.
macro_rules! noto_sans {
    ($dir:literal, $folder:literal, $file:literal, $sha256:literal, $size:literal) => {
        Asset {
            name: concat!("fonts/", $dir, "/", $file),
            url: concat!(
                "https://raw.githubusercontent.com/notofonts/noto-cjk/f8d157532fbfaeda587e826d4cd5b21a49186f7c/Sans/",
                $folder,
                $file
            ),
            sha256: $sha256,
            size: $size,
        }
    };
}

/// A Source Han Serif face (or its licence), from the commit of
/// adobe-fonts/source-han-serif glyphst-fonts pins.
macro_rules! source_han_serif {
    ($dir:literal, $folder:literal, $file:literal, $sha256:literal, $size:literal) => {
        Asset {
            name: concat!("fonts/", $dir, "/", $file),
            url: concat!(
                "https://raw.githubusercontent.com/adobe-fonts/source-han-serif/7889f11bf31170b5d092a083b357c8c8130f89e0/",
                $folder,
                $file
            ),
            sha256: $sha256,
            size: $size,
        }
    };
}

/// The faces translation into Chinese (Simplified) sets text in, and their licences.
const CJK_FONTS_SC: &[Asset] = &[
    noto_sans!(
        "noto-sans-cjk-sc",
        "OTF/SimplifiedChinese/",
        "NotoSansCJKsc-Regular.otf",
        "2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b",
        16_437_364
    ),
    noto_sans!(
        "noto-sans-cjk-sc",
        "OTF/SimplifiedChinese/",
        "NotoSansCJKsc-Bold.otf",
        "b5f0d1a190a7f9b43c310a8850630af12553df32c4c050543f9059732d9b4c0a",
        17_002_248
    ),
    noto_sans!(
        "noto-sans-cjk-sc",
        "",
        "LICENSE",
        "6a73f9541c2de74158c0e7cf6b0a58ef774f5a780bf191f2d7ec9cc53efe2bf2",
        4_301
    ),
    source_han_serif!(
        "source-han-serif-sc",
        "OTF/SimplifiedChinese/",
        "SourceHanSerifSC-Regular.otf",
        "78aa7a328fd974df2d688c8a9fd74a33d8334dfa84ab24d9d11efb2ffc464117",
        24_543_332
    ),
    source_han_serif!(
        "source-han-serif-sc",
        "OTF/SimplifiedChinese/",
        "SourceHanSerifSC-Bold.otf",
        "706b8c0de2deff6cbc0c87e2cdedfd33a78b7ffd76cebb4549012f197ba611fe",
        25_521_732
    ),
    source_han_serif!(
        "source-han-serif-sc",
        "",
        "LICENSE.txt",
        "9ff5bb567e1b92c801fc1069e5fbf992ff8efccacb9db94e5959a5b3ba9bb903",
        4_463
    ),
];

/// The faces translation into Chinese (Traditional) sets text in, and their licences.
const CJK_FONTS_TC: &[Asset] = &[
    noto_sans!(
        "noto-sans-cjk-tc",
        "OTF/TraditionalChinese/",
        "NotoSansCJKtc-Regular.otf",
        "dce08bd4fd91aa8aa76ed8fea4b694c2dfb8550f67871e326843212ddbeb88b4",
        16_435_884
    ),
    noto_sans!(
        "noto-sans-cjk-tc",
        "OTF/TraditionalChinese/",
        "NotoSansCJKtc-Bold.otf",
        "3ee160e5015106e3ec1a394301df54fa9bbbf8a251519984aec5c0abc50840c0",
        17_000_768
    ),
    noto_sans!(
        "noto-sans-cjk-tc",
        "",
        "LICENSE",
        "6a73f9541c2de74158c0e7cf6b0a58ef774f5a780bf191f2d7ec9cc53efe2bf2",
        4_301
    ),
    source_han_serif!(
        "source-han-serif-tc",
        "OTF/TraditionalChinese/",
        "SourceHanSerifTC-Regular.otf",
        "659c9606d8fda03372247b99ec5e5bbc5a034e06a70d80fb00546772c582d3e5",
        24_542_148
    ),
    source_han_serif!(
        "source-han-serif-tc",
        "OTF/TraditionalChinese/",
        "SourceHanSerifTC-Bold.otf",
        "5dfc336214dc81d9bfed14c668cdce99b55078bfee60833d50f5445b8fa944b3",
        25_520_548
    ),
    source_han_serif!(
        "source-han-serif-tc",
        "",
        "LICENSE.txt",
        "9ff5bb567e1b92c801fc1069e5fbf992ff8efccacb9db94e5959a5b3ba9bb903",
        4_463
    ),
];

/// The faces translation into Japanese sets text in, and their licences.
const CJK_FONTS_JP: &[Asset] = &[
    noto_sans!(
        "noto-sans-cjk-jp",
        "OTF/Japanese/",
        "NotoSansCJKjp-Regular.otf",
        "68a3fc98800b2a27b371f2fb79991daf3633bd89309d4ffaa6946fd587f375b5",
        16_467_736
    ),
    noto_sans!(
        "noto-sans-cjk-jp",
        "OTF/Japanese/",
        "NotoSansCJKjp-Bold.otf",
        "e53dcb0dcb2922e45d01aae1ebd2f382bb81d4229b18b6b883bd170678af1f76",
        17_032_620
    ),
    noto_sans!(
        "noto-sans-cjk-jp",
        "",
        "LICENSE",
        "6a73f9541c2de74158c0e7cf6b0a58ef774f5a780bf191f2d7ec9cc53efe2bf2",
        4_301
    ),
    source_han_serif!(
        "source-han-serif-jp",
        "OTF/Japanese/",
        "SourceHanSerif-Regular.otf",
        "066dfd35dbcb96f953ebd72eb182169c3cec6b7c0f18be22f1b6e3936ac6ac20",
        24_574_024
    ),
    source_han_serif!(
        "source-han-serif-jp",
        "OTF/Japanese/",
        "SourceHanSerif-Bold.otf",
        "ae6cdb0ede937fb1fec5371d352569b7f0c39e3532b0337e1cd0ee0397571467",
        25_552_420
    ),
    source_han_serif!(
        "source-han-serif-jp",
        "",
        "LICENSE.txt",
        "9ff5bb567e1b92c801fc1069e5fbf992ff8efccacb9db94e5959a5b3ba9bb903",
        4_463
    ),
];

/// The faces translation into Korean sets text in, and their licences.
const CJK_FONTS_KR: &[Asset] = &[
    noto_sans!(
        "noto-sans-cjk-kr",
        "OTF/Korean/",
        "NotoSansCJKkr-Regular.otf",
        "6bcb2a0703aa137e874fc2dffa85f6c21ba9a67fa329e81b8c801663af7e992a",
        16_433_112
    ),
    noto_sans!(
        "noto-sans-cjk-kr",
        "OTF/Korean/",
        "NotoSansCJKkr-Bold.otf",
        "26d0c6748500a0444844280b308f5b62c7ae92ac6c6ac88148e502dd211eb52a",
        16_997_996
    ),
    noto_sans!(
        "noto-sans-cjk-kr",
        "",
        "LICENSE",
        "6a73f9541c2de74158c0e7cf6b0a58ef774f5a780bf191f2d7ec9cc53efe2bf2",
        4_301
    ),
    source_han_serif!(
        "source-han-serif-kr",
        "OTF/Korean/",
        "SourceHanSerifK-Regular.otf",
        "04060f03c5e998bfce5bc46896bf996974a3b4186aa30c434f8d84ac98afb6db",
        24_539_840
    ),
    source_han_serif!(
        "source-han-serif-kr",
        "OTF/Korean/",
        "SourceHanSerifK-Bold.otf",
        "50f79d398c0f895bcd3c84c4aad294c792659036f1ac9fcef5f2aac8784cb494",
        25_518_236
    ),
    source_han_serif!(
        "source-han-serif-kr",
        "",
        "LICENSE.txt",
        "9ff5bb567e1b92c801fc1069e5fbf992ff8efccacb9db94e5959a5b3ba9bb903",
        4_463
    ),
];

/// The model `id`.
pub fn entry(id: &str) -> Option<&'static ModelEntry> {
    CATALOG.iter().find(|entry| entry.id == id)
}

/// The models for `task`.
pub fn for_task(task: Task) -> impl Iterator<Item = &'static ModelEntry> {
    CATALOG.iter().filter(move |entry| entry.task == task)
}

/// The model `task` uses until another is chosen.
pub fn default_for(task: Task) -> &'static ModelEntry {
    for_task(task)
        .find(|entry| entry.default)
        .expect("every task has a default model")
}

/// Whether every file of `entry` is downloaded whole.
pub fn is_installed(entry: &ModelEntry) -> bool {
    let dir = entry.task.files_dir();
    entry
        .files
        .iter()
        .all(|file| crate::install::file_ready(&dir, file))
}

/// Deletes `entry`'s files, but not those another installed model shares
/// (PP-OCRv6 small and medium share a dictionary).
pub fn remove(entry: &ModelEntry) -> std::io::Result<()> {
    let dir = entry.task.files_dir();
    let shared: Vec<&str> = CATALOG
        .iter()
        .filter(|other| {
            other.id != entry.id && other.task.files_dir() == dir && is_installed(other)
        })
        .flat_map(|other| other.files.iter().map(|file| file.name))
        .collect();
    let own: Vec<Asset> = entry
        .files
        .iter()
        .filter(|file| !shared.contains(&file.name))
        .copied()
        .collect();
    crate::install::remove_files(&dir, &own)
}

/// Bytes on disk under every task's folder: models, engines and partial
/// downloads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiskUsage {
    pub models: u64,
    pub engines: u64,
    pub partial: u64,
}

pub fn disk_usage() -> DiskUsage {
    let mut roots: Vec<PathBuf> = Task::ALL
        .iter()
        .filter(|task| **task != Task::Fonts)
        .map(|task| task.root())
        .collect();
    roots.dedup();
    let mut usage = DiskUsage::default();
    usage.models += crate::install::folder_size(&Task::Fonts.files_dir().join("fonts"));
    for root in roots {
        usage.models += crate::install::folder_size(&root.join("models"));
        usage.engines += crate::install::folder_size(&root.join("engines"));
        usage.partial += crate::install::folder_size(&root.join("downloads"));
    }
    usage
}

macro_rules! oar {
    ($name:literal, $sha256:literal, $size:literal) => {
        Asset {
            name: $name,
            url: concat!(
                "https://github.com/GreatV/oar-ocr/releases/download/v0.7.0/",
                $name
            ),
            sha256: $sha256,
            size: $size,
        }
    };
}

pub const PP_OCRV6_TINY_DET: Asset = oar!(
    "pp-ocrv6_tiny_det.onnx",
    "193bab7a04fca699a6c82e6abb5b81bdb28177f0abd4062552b04908dafb19f8",
    1_780_590
);
pub const PP_OCRV6_TINY_REC: Asset = oar!(
    "pp-ocrv6_tiny_rec.onnx",
    "9ef676d6ed3c88256a2d92c640c44f25b0c40947e111b14b8be8f594091563e6",
    4_462_639
);
pub const PP_OCRV6_TINY_DICT: Asset = oar!(
    "ppocrv6_tiny_dict.txt",
    "c5cbe34ef40c29c4df07ed012bf96569cb69a2d2a01a07027e9f13cb832bd9cd",
    27_156
);
pub const PP_OCRV6_SMALL_DET: Asset = oar!(
    "pp-ocrv6_small_det.onnx",
    "d73e0058b7a8086bbd57f3d10b8bcd4ff95363f67e06e2762b5e814fe9c9410e",
    9_880_512
);
pub const PP_OCRV6_SMALL_REC: Asset = oar!(
    "pp-ocrv6_small_rec.onnx",
    "5435fd747c9e0efe15a96d0b378d5bd157e9492ed8fd80edf08f30d02fa24634",
    21_159_378
);
pub const PP_OCRV6_MEDIUM_DET: Asset = oar!(
    "pp-ocrv6_medium_det.onnx",
    "eb13b44b25bb36f89528b68720af8a61d9cf381176107f465db1757b65d086e1",
    62_032_837
);
pub const PP_OCRV6_MEDIUM_REC: Asset = oar!(
    "pp-ocrv6_medium_rec.onnx",
    "9c09abf0957f7968c7586464b7397b84ad2387a0497a351af40e9acc71b673ba",
    76_554_979
);
/// The dictionary small and medium share.
pub const PP_OCRV6_DICT: Asset = oar!(
    "ppocrv6_dict.txt",
    "b5f2bfe2bdd9448429e3e82b51c789775d9b42f2403d082b00662eb77e401c5d",
    74_947
);

macro_rules! indra {
    ($name:literal, $sha256:literal, $size:literal) => {
        Asset {
            name: concat!("indra-v1.1.0/", $name),
            url: concat!(
                "https://huggingface.co/datalab-to/indra-ocr/resolve/bd02167fdf2005a19962cc15188f8496a2c87d3b/",
                $name
            ),
            sha256: $sha256,
            size: $size,
        }
    };
}

/// Indra OCR v1.1.0 without its math reader: the line detector, the
/// recognizers by script, the script and orientation classifiers, the
/// reading-order model and the language models.
pub const INDRA: &[Asset] = &[
    indra!(
        "det.onnx",
        "f2bd4cba23e9c328cf7ec824634e423e191a2c6aacbbf35c62bd79dd70f62f59",
        389_948
    ),
    indra!(
        "rec.onnx",
        "475972a2710be4a3cbfeb2bc7f3624ffd563de07de42476fc7a290d4e1ece484",
        5_042_161
    ),
    indra!(
        "rec_cjk.onnx",
        "4a035516a17e62cdf33a4fe2d88077654cb8841cff024685cdef4e351694ef50",
        10_176_018
    ),
    indra!(
        "rec_rtl.onnx",
        "468796bd684486a8388c30c8b4ed2a26e3eeb1f8ff02831c24c2e12c79a2a834",
        4_812_710
    ),
    indra!(
        "rec_world.onnx",
        "c8d1e59f21b04966f179d2d2d8e22b230d22110b3f89855bbbf2624a58c40043",
        5_627_990
    ),
    indra!(
        "script.onnx",
        "b6ce4e926e19c243faaf37bd46880398bf2cb4ae56a29e1177a3b017c1280e36",
        191_630
    ),
    indra!(
        "order.onnx",
        "a42db2e8b536ad3d84045a4b7f9b0a72667cf8be4aa02d63f7b38004719fb180",
        2_973_138
    ),
    indra!(
        "orient.onnx",
        "20eaf670f55a7cf9facfe1bdd6c7b8e40be75bbf4d48a3df43f66aa3bbd874fd",
        371_802
    ),
    indra!(
        "meta.json",
        "4ce4c94b46f5d644033c02e718322014ff4081270f385038c571912b56696339",
        211_869
    ),
    indra!(
        "lm.npz",
        "6c0fb618a363a0acdd05d4f5b0edd9e8146d2d990541b87f6130bbfe17cea58d",
        15_695_957
    ),
    indra!(
        "lm_cjk.npz",
        "6d64f13f4f90f4e529bd63340c6df76238d007feaef2abb903d8cd28d92bc2f3",
        18_955_160
    ),
    indra!(
        "lm_world.npz",
        "34df5e74a2fcbb9cf5e7f360a559375ffa2e04d2d2943a42308c177bcb444d5d",
        11_641_215
    ),
];

macro_rules! hugging_face {
    ($repo:literal, $revision:literal, $folder:literal, $name:literal, $sha256:literal, $size:literal) => {
        Asset {
            name: concat!($folder, "/", $name),
            url: concat!(
                "https://huggingface.co/",
                $repo,
                "/resolve/",
                $revision,
                "/",
                $name
            ),
            sha256: $sha256,
            size: $size,
        }
    };
}

/// PaddleOCR-VL 1.5 (its configuration, tokenizer and weights), then the
/// PP-DocLayoutV3 layout detector that finds the regions it reads.
pub const PADDLEOCR_VL: &[Asset] = &[
    hugging_face!(
        "PaddlePaddle/PaddleOCR-VL-1.5",
        "2a4195faa5e7914c12f2fc601d72c81caf8d2da5",
        "paddleocr-vl-1.5",
        "config.json",
        "ce7f4565f8b1db78532ad5d1b9ebe55c2139d49bd4cb04778b580a08a598f171",
        2_059
    ),
    hugging_face!(
        "PaddlePaddle/PaddleOCR-VL-1.5",
        "2a4195faa5e7914c12f2fc601d72c81caf8d2da5",
        "paddleocr-vl-1.5",
        "preprocessor_config.json",
        "111872ab1e8bb7fd040ac5087bfced7ab8f011f02139b088cba294964c3b1d0e",
        641
    ),
    hugging_face!(
        "PaddlePaddle/PaddleOCR-VL-1.5",
        "2a4195faa5e7914c12f2fc601d72c81caf8d2da5",
        "paddleocr-vl-1.5",
        "tokenizer.json",
        "c8a215a59183d0d0781adc33bacd3ce6162716f7fd568fb30234a74d69803a7d",
        11_189_060
    ),
    hugging_face!(
        "PaddlePaddle/PaddleOCR-VL-1.5",
        "2a4195faa5e7914c12f2fc601d72c81caf8d2da5",
        "paddleocr-vl-1.5",
        "model.safetensors",
        "d557c9d8997ae57ed3b1b33bdf347be878cc335687f32ca105341c16973f8958",
        1_917_255_968
    ),
    hugging_face!(
        "PaddlePaddle/PP-DocLayoutV3_safetensors",
        "e4f489dc536556fc1be02b973ba06756e0d4a2ac",
        "pp-doclayoutv3",
        "config.json",
        "3cf834b91d23a756b1519bce4db42c09e852f3e35c35092dd5a3e253a50c071a",
        2_460
    ),
    hugging_face!(
        "PaddlePaddle/PP-DocLayoutV3_safetensors",
        "e4f489dc536556fc1be02b973ba06756e0d4a2ac",
        "pp-doclayoutv3",
        "model.safetensors",
        "5ea422c6cc5fe759a47e1357c35639b58173508e025a3131cbe4b6ac59e2b85e",
        133_270_468
    ),
];

/// The text half of EmbeddingGemma 2 at 8 bits (never the F16 files: its
/// activations overflow float16 into NaN).
pub const EMBEDDINGGEMMA: Asset = Asset {
    name: "embeddinggemma-2-Q8_0.gguf",
    url: "https://huggingface.co/ggml-org/embeddinggemma-2-GGUF/resolve/bfcd298762cc34d0357ece5ebdd31791a3a374d8/embeddinggemma-2-Q8_0.gguf",
    sha256: "2188ac1deca4b77dffefd603c2776a9d76d9d74ec01841392982ebb840b09135",
    size: 309_855_456,
};

pub const QWEN3_EMBEDDING_SMALL: Asset = Asset {
    name: "Qwen3-Embedding-0.6B-Q8_0.gguf",
    url: "https://huggingface.co/Qwen/Qwen3-Embedding-0.6B-GGUF/resolve/370f27d7550e0def9b39c1f16d3fbaa13aa67728/Qwen3-Embedding-0.6B-Q8_0.gguf",
    sha256: "06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439",
    size: 639_150_592,
};

/// At 4 bits (Q4_K_M): 2.5 GB instead of 4.3 for little loss.
pub const QWEN3_EMBEDDING_LARGE: Asset = Asset {
    name: "Qwen3-Embedding-4B-Q4_K_M.gguf",
    url: "https://huggingface.co/Qwen/Qwen3-Embedding-4B-GGUF/resolve/f4602530db1d980e16da9d7d3a70294cf5c190be/Qwen3-Embedding-4B-Q4_K_M.gguf",
    sha256: "2b0cf8f17b4c723c27303015383c27ec4bf2d8314bb677d05e920dd70bb0f16b",
    size: 2_496_703_776,
};

/// The reranker: a cross-encoder that rereads the best passages against
/// the query, run by the same engine. bge-reranker-v2-m3 (multilingual)
/// over Qwen3-Reranker-0.6B: on the labelled set it put 7 of 8
/// paraphrases first from keyword results (Qwen3: 6), kept the hybrid
/// search's order (Qwen3 pushed one down), and reread 30 passages in
/// about 390 ms against 1,400 on the Arc.
pub const BGE_RERANKER: Asset = Asset {
    name: "bge-reranker-v2-m3-Q8_0.gguf",
    url: "https://huggingface.co/gpustack/bge-reranker-v2-m3-GGUF/resolve/3093af03b1a635e67b084b1d8c03c5f5e020fd05/bge-reranker-v2-m3-Q8_0.gguf",
    sha256: "a43c7c9b11a4c1517e5bf95151960e1621d1b72f7a493364b01e386cf1aaa1d3",
    size: 635_676_416,
};

pub const QWEN3_RERANKER: Asset = Asset {
    name: "qwen3-reranker-0.6b-q8_0.gguf",
    url: "https://huggingface.co/ggml-org/Qwen3-Reranker-0.6B-Q8_0-GGUF/resolve/a02f48bb4f057028298c21fa033da2b30d7742d5/qwen3-reranker-0.6b-q8_0.gguf",
    sha256: "22c9979ce4fbcdc5acdc310c6641c32797eff1aa980b8f7a2db8a8ea23429a48",
    size: 639_153_184,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pin_is_whole_and_every_task_has_one_default() {
        for entry in CATALOG {
            assert!(!entry.files.is_empty(), "{}", entry.id);
            for file in entry.files {
                assert_eq!(file.sha256.len(), 64, "{}", file.name);
                assert!(file.size > 0, "{}", file.name);
                let leaf = file.name.rsplit('/').next().unwrap();
                assert!(file.url.ends_with(leaf), "{}", file.url);
                assert!(!file.url.contains("F16"), "{}", file.url);
            }
        }
        for task in Task::ALL {
            assert_eq!(for_task(task).filter(|entry| entry.default).count(), 1);
        }
        assert_eq!(default_for(Task::TextRecognition).id, "pp-ocrv6-small");
        // Indra's files keep to their folder.
        assert!(
            INDRA
                .iter()
                .all(|file| file.name.starts_with("indra-v1.1.0/"))
        );
    }

    /// The CJK pack is glyphst-fonts' CJK faces, byte for byte, when its
    /// checkout is beside this repository.
    #[test]
    fn the_cjk_fonts_are_the_checkouts() {
        let checkout = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fonts");
        let Ok(manifest) = std::fs::read_to_string(checkout.join("fonts/manifest.txt")) else {
            eprintln!("no glyphst-fonts checkout; skipping");
            return;
        };
        let faces: Vec<(&str, &str)> = manifest
            .lines()
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
            .filter_map(|line| {
                let fields: Vec<&str> = line.split(" | ").collect();
                Some((fields.get(2)?.trim(), fields.get(6)?.trim()))
            })
            .filter(|(file, _)| !file.starts_with("fonts/lilex/"))
            .collect();
        let fonts: Vec<(&str, &str)> = CJK_FONTS_SC
            .iter()
            .filter(|file| file.name.ends_with(".otf"))
            .map(|file| (file.name, file.sha256))
            .collect();
        assert_eq!(fonts, faces);
    }
}

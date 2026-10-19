//! A small Zotero data folder written from scratch in Zotero's schema (the
//! tables the import reads), for tests and the screenshots of frame 04j:
//! papers in nested collections, tags (some automatic, one coloured),
//! stored PDFs with highlights, an ink mark, notes, a missing file and an
//! item in the trash.

use std::{fs, path::Path};

use anyhow::Result;
use rusqlite::{Connection, params};

/// The tables (and columns) of `zotero.sqlite` the import reads.
pub const SCHEMA: &str = r#"
    CREATE TABLE libraries (libraryID INTEGER PRIMARY KEY, type TEXT);
    CREATE TABLE itemTypes (itemTypeID INTEGER PRIMARY KEY, typeName TEXT);
    CREATE TABLE items (itemID INTEGER PRIMARY KEY, itemTypeID INT, libraryID INT, key TEXT);
    CREATE TABLE fields (fieldID INTEGER PRIMARY KEY, fieldName TEXT);
    CREATE TABLE itemDataValues (valueID INTEGER PRIMARY KEY, value);
    CREATE TABLE itemData (itemID INT, fieldID INT, valueID INT);
    CREATE TABLE creators (creatorID INTEGER PRIMARY KEY, firstName TEXT, lastName TEXT, fieldMode INT);
    CREATE TABLE creatorTypes (creatorTypeID INTEGER PRIMARY KEY, creatorType TEXT);
    CREATE TABLE itemCreators (itemID INT, creatorID INT, creatorTypeID INT, orderIndex INT);
    CREATE TABLE tags (tagID INTEGER PRIMARY KEY, name TEXT);
    CREATE TABLE itemTags (itemID INT, tagID INT, type INT);
    CREATE TABLE collections (collectionID INTEGER PRIMARY KEY, collectionName TEXT, parentCollectionID INT, libraryID INT, key TEXT);
    CREATE TABLE collectionItems (collectionID INT, itemID INT, orderIndex INT);
    CREATE TABLE deletedCollections (collectionID INT);
    CREATE TABLE itemAttachments (itemID INT, parentItemID INT, linkMode INT, contentType TEXT, path TEXT);
    CREATE TABLE itemNotes (itemID INT, parentItemID INT, note TEXT, title TEXT);
    CREATE TABLE itemAnnotations (itemID INT, parentItemID INT, type INT, authorName TEXT, text TEXT, comment TEXT, color TEXT, pageLabel TEXT, sortIndex TEXT, position TEXT, isExternal INT);
    CREATE TABLE deletedItems (itemID INT, dateDeleted TEXT);
    CREATE TABLE relationPredicates (predicateID INTEGER PRIMARY KEY, predicate TEXT);
    CREATE TABLE itemRelations (itemID INT, predicateID INT, object TEXT);
    CREATE TABLE syncedSettings (setting TEXT, libraryID INT, value TEXT);
"#;

struct Paper {
    key: &'static str,
    kind: &'static str,
    title: &'static str,
    date: &'static str,
    authors: &'static [(&'static str, &'static str)],
    extra: &'static [(&'static str, &'static str)],
    collection: Option<i64>,
    tags: &'static [(&'static str, bool)],
    /// The stored file's name, and whether it is there.
    file: Option<(&'static str, &'static str, bool)>,
    /// Highlights: page, rect, colour, text, comment.
    highlights: &'static [(usize, [f32; 4], &'static str, &'static str, &'static str)],
    ink: bool,
    note: Option<&'static str>,
    trashed: bool,
}

const PAPERS: &[Paper] = &[
    Paper {
        key: "PAPER001",
        kind: "preprint",
        title: "Attention Is All You Need",
        date: "2017-06-12 2017-06-12",
        authors: &[
            ("Ashish", "Vaswani"),
            ("Noam", "Shazeer"),
            ("Niki", "Parmar"),
        ],
        extra: &[
            ("archiveID", "arXiv:1706.03762"),
            ("repository", "arXiv"),
            ("extra", "Citation Key: vaswani2017"),
        ],
        collection: Some(2),
        tags: &[
            ("transformers", false),
            ("Computer Science - Computation and Language", true),
        ],
        file: Some(("ATTACH01", "attention.pdf", true)),
        highlights: &[
            (
                0,
                [72.0, 700.0, 400.0, 712.0],
                "#ffd400",
                "Scaled dot-product attention",
                "the key idea",
            ),
            (
                0,
                [72.0, 650.0, 360.0, 662.0],
                "#5fb236",
                "Multi-head attention",
                "",
            ),
        ],
        ink: false,
        note: Some("<h1>Reading notes</h1><p>Compare the position encoding with <b>RoPE</b>.</p>"),
        trashed: false,
    },
    Paper {
        key: "PAPER002",
        kind: "preprint",
        title: "BERT: Pre-training of Deep Bidirectional Transformers for Language Understanding",
        date: "2018-10-11",
        authors: &[("Jacob", "Devlin"), ("Ming-Wei", "Chang")],
        extra: &[("archiveID", "arXiv:1810.04805")],
        collection: Some(2),
        tags: &[("transformers", false), ("nlp", false)],
        file: Some(("ATTACH02", "bert.pdf", true)),
        highlights: &[(
            0,
            [72.0, 500.0, 300.0, 512.0],
            "#2ea8e5",
            "masked language model",
            "",
        )],
        ink: true,
        note: None,
        trashed: false,
    },
    Paper {
        key: "PAPER003",
        kind: "journalArticle",
        title: "Denoising Diffusion Probabilistic Models",
        date: "2020",
        authors: &[("Jonathan", "Ho"), ("Ajay", "Jain"), ("Pieter", "Abbeel")],
        extra: &[("DOI", "10.5555/3495724.3496298")],
        collection: Some(3),
        tags: &[("diffusion", false)],
        file: Some(("ATTACH03", "ddpm.pdf", true)),
        highlights: &[],
        ink: false,
        note: None,
        trashed: false,
    },
    Paper {
        key: "PAPER004",
        kind: "conferencePaper",
        title: "Retrieval-Augmented Generation for Knowledge-Intensive NLP Tasks",
        date: "2020",
        authors: &[("Patrick", "Lewis"), ("Ethan", "Perez")],
        extra: &[],
        collection: Some(4),
        tags: &[("rag", false), ("nlp", false)],
        file: Some(("8XK2QF7A", "rag.pdf", false)),
        highlights: &[],
        ink: false,
        note: Some("<p>Check the retriever's recall on long questions.</p>"),
        trashed: false,
    },
    Paper {
        key: "BOOK0001",
        kind: "book",
        title: "Structure and Interpretation of Computer Programs",
        date: "1996",
        authors: &[("Harold", "Abelson"), ("Gerald Jay", "Sussman")],
        extra: &[("ISBN", "9780262510875"), ("publisher", "MIT Press")],
        collection: Some(5),
        tags: &[("programming", false)],
        file: None,
        highlights: &[],
        ink: false,
        note: None,
        trashed: false,
    },
    Paper {
        key: "PAPER005",
        kind: "report",
        title: "An Old Draft",
        date: "2015",
        authors: &[("Ada", "Lovelace")],
        extra: &[],
        collection: None,
        tags: &[],
        file: None,
        highlights: &[],
        ink: false,
        note: None,
        trashed: true,
    },
];

/// Writes the sample into `dir` (`zotero.sqlite` and `storage/`).
pub fn write(dir: &Path) -> Result<()> {
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir)?;
    let connection = Connection::open(dir.join("zotero.sqlite"))?;
    connection.execute_batch(SCHEMA)?;
    connection.execute_batch(
        r##"
        INSERT INTO libraries VALUES (1, 'user');
        INSERT INTO itemTypes VALUES (1, 'preprint'), (2, 'book'), (3, 'attachment'), (4, 'note'),
            (5, 'annotation'), (6, 'journalArticle'), (7, 'conferencePaper'), (8, 'report');
        INSERT INTO fields VALUES (1, 'title'), (2, 'date'), (3, 'archiveID'), (4, 'DOI'),
            (5, 'extra'), (6, 'repository'), (7, 'ISBN'), (8, 'publisher');
        INSERT INTO creatorTypes VALUES (1, 'author');
        INSERT INTO collections VALUES
            (1, 'Machine learning', NULL, 1, 'COLL0001'), (2, 'LLMs', 1, 1, 'COLL0002'),
            (3, 'Diffusion', 1, 1, 'COLL0003'), (4, 'Reading list', NULL, 1, 'COLL0004'),
            (5, 'Programming', NULL, 1, 'COLL0005');
        INSERT INTO relationPredicates VALUES (1, 'dc:relation');
        INSERT INTO syncedSettings VALUES ('tagColors', 1, '[{"name":"transformers","color":"#2EA8E5"}]');
        "##,
    )?;
    let mut ids = Ids::default();
    for paper in PAPERS {
        let item = ids.item(&connection, type_id(paper.kind), paper.key)?;
        ids.field(&connection, item, 1, paper.title)?;
        ids.field(&connection, item, 2, paper.date)?;
        for (name, value) in paper.extra {
            let field = match *name {
                "archiveID" => 3,
                "DOI" => 4,
                "extra" => 5,
                "repository" => 6,
                "ISBN" => 7,
                _ => 8,
            };
            ids.field(&connection, item, field, value)?;
        }
        for (order, (first, last)) in paper.authors.iter().enumerate() {
            ids.creators += 1;
            connection.execute(
                "INSERT INTO creators VALUES (?1, ?2, ?3, 0)",
                params![ids.creators, first, last],
            )?;
            connection.execute(
                "INSERT INTO itemCreators VALUES (?1, ?2, 1, ?3)",
                params![item, ids.creators, order as i64],
            )?;
        }
        if let Some(collection) = paper.collection {
            connection.execute(
                "INSERT INTO collectionItems VALUES (?1, ?2, 0)",
                params![collection, item],
            )?;
        }
        for (name, automatic) in paper.tags {
            let tag = ids.tag(&connection, name)?;
            connection.execute(
                "INSERT INTO itemTags VALUES (?1, ?2, ?3)",
                params![item, tag, *automatic as i64],
            )?;
        }
        if let Some((key, name, present)) = paper.file {
            let attachment = ids.item(&connection, 3, key)?;
            connection.execute(
                "INSERT INTO itemAttachments VALUES (?1, ?2, 0, 'application/pdf', ?3)",
                params![attachment, item, format!("storage:{name}")],
            )?;
            if present {
                fs::create_dir_all(dir.join("storage").join(key))?;
                fs::write(dir.join("storage").join(key).join(name), pdf(paper.title))?;
            }
            let marks = paper
                .highlights
                .iter()
                .map(|(page, rect, color, text, comment)| {
                    (1, *page, *rect, *color, *text, *comment)
                })
                .chain(
                    paper
                        .ink
                        .then_some((4, 0, [80.0, 300.0, 200.0, 360.0], "#ff6666", "", "")),
                );
            for (index, (kind, page, rect, color, text, comment)) in marks.enumerate() {
                let annotation = ids.item(&connection, 5, &format!("ANN{item:02}{index:02}"))?;
                let position = format!(
                    r#"{{"pageIndex":{page},"rects":[[{},{},{},{}]]}}"#,
                    rect[0], rect[1], rect[2], rect[3]
                );
                connection.execute(
                    "INSERT INTO itemAnnotations VALUES (?1, ?2, ?3, '', ?4, ?5, ?6, '1', ?7, ?8, 0)",
                    params![
                        annotation,
                        attachment,
                        kind,
                        text,
                        comment,
                        color,
                        format!("{page:05}|{index:06}|00000"),
                        position
                    ],
                )?;
            }
        }
        if let Some(note) = paper.note {
            let child = ids.item(&connection, 4, &format!("NOTE{item:04}"))?;
            connection.execute(
                "INSERT INTO itemNotes VALUES (?1, ?2, ?3, '')",
                params![child, item, note],
            )?;
        }
        if paper.trashed {
            connection.execute(
                "INSERT INTO deletedItems VALUES (?1, '2026-01-01')",
                params![item],
            )?;
        }
    }
    let standalone = ids.item(&connection, 4, "NOTE9999")?;
    connection.execute(
        "INSERT INTO itemNotes VALUES (?1, NULL, ?2, 'Ideas for the survey')",
        params![
            standalone,
            "<h1>Ideas for the survey</h1><ul><li>attention variants</li><li>retrieval</li></ul>"
        ],
    )?;
    // Related items, stored both ways as Zotero does.
    for (from, to) in [("PAPER001", "PAPER002"), ("PAPER002", "PAPER001")] {
        connection.execute(
            "INSERT INTO itemRelations SELECT itemID, 1, ?2 FROM items WHERE key = ?1",
            params![
                from,
                format!("http://zotero.org/users/local/sample/items/{to}")
            ],
        )?;
    }
    Ok(())
}

fn type_id(kind: &str) -> i64 {
    match kind {
        "preprint" => 1,
        "book" => 2,
        "journalArticle" => 6,
        "conferencePaper" => 7,
        _ => 8,
    }
}

#[derive(Default)]
struct Ids {
    items: i64,
    values: i64,
    creators: i64,
    tags: Vec<String>,
}

impl Ids {
    fn item(&mut self, connection: &Connection, kind: i64, key: &str) -> Result<i64> {
        self.items += 1;
        connection.execute(
            "INSERT INTO items VALUES (?1, ?2, 1, ?3)",
            params![self.items, kind, key],
        )?;
        Ok(self.items)
    }

    fn field(&mut self, connection: &Connection, item: i64, field: i64, value: &str) -> Result<()> {
        self.values += 1;
        connection.execute(
            "INSERT INTO itemDataValues VALUES (?1, ?2)",
            params![self.values, value],
        )?;
        connection.execute(
            "INSERT INTO itemData VALUES (?1, ?2, ?3)",
            params![item, field, self.values],
        )?;
        Ok(())
    }

    fn tag(&mut self, connection: &Connection, name: &str) -> Result<i64> {
        if let Some(index) = self.tags.iter().position(|known| known == name) {
            return Ok(index as i64 + 1);
        }
        self.tags.push(name.to_string());
        let id = self.tags.len() as i64;
        connection.execute("INSERT INTO tags VALUES (?1, ?2)", params![id, name])?;
        Ok(id)
    }
}

/// A one-page US Letter PDF with `title` on it.
fn pdf(title: &str) -> Vec<u8> {
    let title = title.replace(['(', ')', '\\'], "");
    let content = format!("BT /F1 18 Tf 72 720 Td ({title}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_string(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len() + 1),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
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

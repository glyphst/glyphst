use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use glyphst_library::{LibraryItemKind, MetadataSource};

use super::*;

const ARXIV_ATOM: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:arxiv="http://arxiv.org/schemas/atom">
  <entry>
    <id>http://arxiv.org/abs/1706.03762v7</id>
    <published>2017-06-12T17:57:34Z</published>
    <title>Attention Is All
      You Need</title>
    <summary>The dominant sequence transduction models are based on complex recurrent networks.</summary>
    <author><name>Ashish Vaswani</name></author>
    <author><name>Noam Shazeer</name></author>
    <arxiv:doi>10.5555/3295222.3295349</arxiv:doi>
    <link title="pdf" href="http://arxiv.org/pdf/1706.03762v7" rel="related" type="application/pdf"/>
    <category term="cs.CL"/>
  </entry>
</feed>"#;

const CROSSREF_WORK: &str = r#"{"status":"ok","message":{
  "DOI":"10.5555/3295222.3295349",
  "title":["Attention is all you need"],
  "author":[{"given":"Ashish","family":"Vaswani"},{"given":"Noam","family":"Shazeer"}],
  "container-title":["Advances in Neural Information Processing Systems"],
  "publisher":"Curran Associates Inc.",
  "type":"proceedings-article",
  "issued":{"date-parts":[[2017,12,4]]},
  "abstract":"<jats:p>The dominant sequence transduction models&amp;more.</jats:p>"
}}"#;

const CROSSREF_SEARCH: &str = r#"{"status":"ok","message":{"items":[
  {"DOI":"10.1/near","title":["Attention is not all you need"],"type":"journal-article"},
  {"DOI":"10.1145/3292500.3330701","title":["Optuna: A Next-generation Hyperparameter Optimization Framework"],
   "author":[{"given":"Takuya","family":"Akiba"}],"type":"proceedings-article",
   "published-print":{"date-parts":[[2019]]}}
]}}"#;

const OPENALEX_WORK: &str = r#"{
  "doi":"https://doi.org/10.1016/J.CELL.2020.01.001",
  "title":"A cell paper",
  "type":"article",
  "publication_year":2020,
  "authorships":[{"author":{"display_name":"Jane Doe"}}],
  "primary_location":{"source":{"display_name":"Cell","host_organization_name":"Elsevier"}},
  "abstract_inverted_index":{"Cells":[0],"divide":[1],"often.":[2]},
  "best_oa_location":{"pdf_url":"https://example.org/cell.pdf"}
}"#;

const OPENLIBRARY_BOOK: &str = r#"{"ISBN:9781449373320":{
  "title":"Designing Data-Intensive Applications",
  "subtitle":"The Big Ideas Behind Reliable, Scalable, and Maintainable Systems",
  "authors":[{"name":"Martin Kleppmann"}],
  "publishers":[{"name":"O'Reilly Media"}],
  "publish_date":"March 2017"
}}"#;

/// A local stand-in for the catalogues: answers by path and query, and
/// counts requests. `busy` answers 429 once for each path that has it.
struct FakeCatalogues {
    endpoints: MetadataEndpoints,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl FakeCatalogues {
    fn start(routes: &[(&str, u16, &'static str)], busy: &[&str]) -> Self {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        let routes = routes
            .iter()
            .map(|(path, status, body)| (path.to_string(), (*status, *body)))
            .collect::<HashMap<_, _>>();
        let busy = Arc::new(Mutex::new(
            busy.iter().map(|path| path.to_string()).collect::<Vec<_>>(),
        ));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        {
            let requests = requests.clone();
            let stop = stop.clone();
            thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    let Ok(Some(request)) =
                        server.recv_timeout(std::time::Duration::from_millis(50))
                    else {
                        continue;
                    };
                    let url = request.url().to_string();
                    requests.lock().unwrap().push(url.clone());
                    let mut busy = busy.lock().unwrap();
                    if let Some(at) = busy.iter().position(|path| url.starts_with(path.as_str())) {
                        busy.remove(at);
                        let response = tiny_http::Response::from_string("slow down")
                            .with_status_code(429)
                            .with_header(
                                tiny_http::Header::from_bytes("Retry-After", "0").unwrap(),
                            );
                        let _ = request.respond(response);
                        continue;
                    }
                    drop(busy);
                    let (status, body) = routes
                        .iter()
                        .filter(|(path, _)| url.starts_with(path.as_str()))
                        .max_by_key(|(path, _)| path.len())
                        .map(|(_, answer)| *answer)
                        .unwrap_or((404, "not found"));
                    let _ = request
                        .respond(tiny_http::Response::from_string(body).with_status_code(status));
                }
            });
        }
        Self {
            endpoints: MetadataEndpoints {
                crossref: format!("{base}/crossref"),
                arxiv: format!("{base}/arxiv"),
                openalex: format!("{base}/openalex"),
                openlibrary: format!("{base}/openlibrary"),
            },
            requests,
            stop,
        }
    }

    fn client(&self) -> MetadataClient {
        MetadataClient::new(self.endpoints.clone(), None).unwrap()
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for FakeCatalogues {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

#[test]
fn an_arxiv_id_is_looked_up_and_completed_from_its_doi() {
    let fake = FakeCatalogues::start(
        &[
            ("/arxiv/api/query", 200, ARXIV_ATOM),
            (
                "/crossref/works/10.5555/3295222.3295349",
                200,
                CROSSREF_WORK,
            ),
        ],
        &[],
    );
    let work = fake
        .client()
        .lookup(&Identifiers {
            arxiv: Some("1706.03762".to_string()),
            ..Identifiers::default()
        })
        .unwrap()
        .unwrap();
    assert_eq!(work.source, MetadataSource::Arxiv);
    assert_eq!(work.title, "Attention Is All You Need");
    assert_eq!(work.creators, vec!["Ashish Vaswani", "Noam Shazeer"]);
    assert_eq!(work.arxiv.as_deref(), Some("1706.03762"));
    assert_eq!(
        work.pdf_url.as_deref(),
        Some("https://arxiv.org/pdf/1706.03762v7")
    );
    assert_eq!(
        work.publication, "Advances in Neural Information Processing Systems",
        "the published venue from crossref"
    );
    assert!(
        fake.requests()[0].contains("id_list=1706.03762"),
        "{:?}",
        fake.requests()
    );
}

#[test]
fn a_doi_comes_from_crossref_with_openalex_filling_the_abstract_gaps() {
    let fake = FakeCatalogues::start(
        &[
            (
                "/crossref/works/10.5555/3295222.3295349",
                200,
                CROSSREF_WORK,
            ),
            ("/openalex/works/doi:", 404, "{}"),
        ],
        &[],
    );
    let work = fake
        .client()
        .doi("10.5555/3295222.3295349")
        .unwrap()
        .unwrap();
    assert_eq!(work.source, MetadataSource::Crossref);
    assert_eq!(work.year, Some(2017));
    assert_eq!(work.kind, Some(LibraryItemKind::Paper));
    assert_eq!(
        work.abstract_text,
        "The dominant sequence transduction models&more."
    );
    assert_eq!(work.publisher, "Curran Associates Inc.");
}

#[test]
fn a_doi_crossref_lacks_is_found_in_openalex() {
    let fake = FakeCatalogues::start(
        &[(
            "/openalex/works/doi:10.1016/j.cell.2020.01.001",
            200,
            OPENALEX_WORK,
        )],
        &[],
    );
    let work = fake
        .client()
        .doi("10.1016/j.cell.2020.01.001")
        .unwrap()
        .unwrap();
    assert_eq!(work.source, MetadataSource::OpenAlex);
    assert_eq!(work.title, "A cell paper");
    assert_eq!(work.abstract_text, "Cells divide often.");
    assert_eq!(work.publication, "Cell");
    assert_eq!(work.doi.as_deref(), Some("10.1016/j.cell.2020.01.001"));
    assert_eq!(
        work.pdf_url.as_deref(),
        Some("https://example.org/cell.pdf")
    );
}

#[test]
fn an_isbn_is_found_in_open_library() {
    let fake = FakeCatalogues::start(&[("/openlibrary/api/books", 200, OPENLIBRARY_BOOK)], &[]);
    let work = fake.client().isbn("9781449373320").unwrap().unwrap();
    assert_eq!(work.kind, Some(LibraryItemKind::Book));
    assert_eq!(
        work.title,
        "Designing Data-Intensive Applications: The Big Ideas Behind Reliable, Scalable, and Maintainable Systems"
    );
    assert_eq!(work.year, Some(2017));
    assert_eq!(work.publisher, "O'Reilly Media");
}

#[test]
fn a_busy_service_is_asked_again_after_its_retry_after() {
    let fake = FakeCatalogues::start(
        &[("/openlibrary/api/books", 200, OPENLIBRARY_BOOK)],
        &["/openlibrary/api/books"],
    );
    let work = fake.client().isbn("9781449373320").unwrap();
    assert!(work.is_some());
    assert_eq!(fake.requests().len(), 2, "{:?}", fake.requests());
}

#[test]
fn unknown_works_are_none_and_a_title_search_needs_the_same_title() {
    let fake = FakeCatalogues::start(
        &[
            ("/crossref/works?", 200, CROSSREF_SEARCH),
            ("/openalex/works?", 200, r#"{"results":[]}"#),
        ],
        &[],
    );
    let client = fake.client();
    assert_eq!(client.doi("10.9999/unknown").unwrap(), None);
    let found = client
        .search_title(
            "Optuna: a next-generation hyperparameter optimization framework",
            Some("Akiba"),
        )
        .unwrap()
        .unwrap();
    assert_eq!(found.doi.as_deref(), Some("10.1145/3292500.3330701"));
    assert_eq!(found.year, Some(2019));
    assert_eq!(client.search_title("Attention", None).unwrap(), None);
}

#[test]
fn abstracts_lose_their_markup() {
    assert_eq!(
        strip_markup(
            "<jats:title>Abstract</jats:title><jats:p>We study <i>x</i> &amp; y.</jats:p>"
        ),
        "We study x & y."
    );
}

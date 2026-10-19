//! Looking a work up by its identifier (or title) in public catalogues:
//! arXiv, Crossref, OpenAlex and Open Library. Blocking; run it on a
//! worker thread.

mod arxiv;
mod crossref;
mod openalex;
mod openlibrary;

use std::{
    collections::HashMap,
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow};
use reqwest::{StatusCode, blocking::Client};

use crate::{Identifiers, WorkMetadata, dedupe::normalize_title};

/// Where each service is, so tests can point them at a local server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataEndpoints {
    pub crossref: String,
    pub arxiv: String,
    pub openalex: String,
    pub openlibrary: String,
}

impl Default for MetadataEndpoints {
    fn default() -> Self {
        Self {
            crossref: "https://api.crossref.org".to_string(),
            arxiv: "https://export.arxiv.org".to_string(),
            openalex: "https://api.openalex.org".to_string(),
            openlibrary: "https://openlibrary.org".to_string(),
        }
    }
}

/// A client for the catalogues, keeping to each one's pace.
pub struct MetadataClient {
    http: Client,
    endpoints: MetadataEndpoints,
    /// Crossref's polite pool: sent only when the user gave an address.
    mailto: Option<String>,
    last_request: Mutex<HashMap<&'static str, Instant>>,
    attempts: u32,
}

impl MetadataClient {
    pub fn new(endpoints: MetadataEndpoints, mailto: Option<String>) -> Result<Self> {
        let http = Client::builder()
            .user_agent(concat!("GlyphstReader/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(15))
            .build()
            .context("failed to start the metadata client")?;
        Ok(Self {
            http,
            endpoints,
            mailto: mailto.filter(|address| address.contains('@')),
            last_request: Mutex::new(HashMap::new()),
            attempts: 3,
        })
    }

    pub fn endpoints(&self) -> &MetadataEndpoints {
        &self.endpoints
    }

    /// Looks the identifiers up in the order Zotero does: arXiv, then the
    /// DOI, then the ISBN. `None` when no service knows the work.
    pub fn lookup(&self, identifiers: &Identifiers) -> Result<Option<WorkMetadata>> {
        let mut errors = Vec::new();
        if let Some(id) = &identifiers.arxiv {
            match self.arxiv(id) {
                Ok(Some(work)) => return Ok(Some(self.complete_from_doi(work))),
                Ok(None) => {}
                Err(error) => errors.push(error),
            }
        }
        if let Some(doi) = &identifiers.doi {
            match self.doi(doi) {
                Ok(Some(work)) => return Ok(Some(work)),
                Ok(None) => {}
                Err(error) => errors.push(error),
            }
        }
        if let Some(isbn) = &identifiers.isbn {
            match self.isbn(isbn) {
                Ok(Some(work)) => return Ok(Some(work)),
                Ok(None) => {}
                Err(error) => errors.push(error),
            }
        }
        match errors.into_iter().next() {
            Some(error) => Err(error),
            None => Ok(None),
        }
    }

    pub fn arxiv(&self, id: &str) -> Result<Option<WorkMetadata>> {
        let url = format!(
            "{}/api/query?id_list={}&max_results=1",
            self.endpoints.arxiv, id
        );
        let body = self.get("arxiv", &url)?;
        match body {
            Some(body) => arxiv::parse(&body),
            None => Ok(None),
        }
    }

    /// Crossref by DOI, falling back to OpenAlex.
    pub fn doi(&self, doi: &str) -> Result<Option<WorkMetadata>> {
        let url = format!(
            "{}/works/{}{}",
            self.endpoints.crossref,
            doi,
            self.mailto_query("?")
        );
        let crossref = match self.get("crossref", &url) {
            Ok(Some(body)) => crossref::parse_work(&body)?,
            Ok(None) => None,
            Err(error) => {
                eprintln!("crossref lookup failed: {error:#}");
                None
            }
        };
        let mut work = match crossref {
            Some(work) => work,
            None => match self.openalex_doi(doi)? {
                Some(work) => return Ok(Some(work)),
                None => return Ok(None),
            },
        };
        // Crossref rarely has an abstract or an open PDF; OpenAlex often does.
        if (work.abstract_text.is_empty() || work.pdf_url.is_none())
            && let Ok(Some(extra)) = self.openalex_doi(doi)
        {
            if work.abstract_text.is_empty() {
                work.abstract_text = extra.abstract_text;
            }
            if work.pdf_url.is_none() {
                work.pdf_url = extra.pdf_url;
            }
        }
        Ok(Some(work))
    }

    pub fn openalex_doi(&self, doi: &str) -> Result<Option<WorkMetadata>> {
        let url = format!("{}/works/doi:{}", self.endpoints.openalex, doi);
        match self.get("openalex", &url)? {
            Some(body) => openalex::parse_work(&body),
            None => Ok(None),
        }
    }

    pub fn isbn(&self, isbn: &str) -> Result<Option<WorkMetadata>> {
        let url = format!(
            "{}/api/books?bibkeys=ISBN:{isbn}&format=json&jscmd=data",
            self.endpoints.openlibrary
        );
        match self.get("openlibrary", &url)? {
            Some(body) => openlibrary::parse(&body, isbn),
            None => Ok(None),
        }
    }

    /// Finds a work by its title (and a creator's surname when known), for
    /// files with no identifier: the best match must have the same title.
    pub fn search_title(&self, title: &str, surname: Option<&str>) -> Result<Option<WorkMetadata>> {
        let wanted = normalize_title(title);
        if wanted.split_whitespace().count() < 3 {
            return Ok(None);
        }
        let query = match surname {
            Some(surname) => format!("{title} {surname}"),
            None => title.to_string(),
        };
        let url = format!(
            "{}/works?query.bibliographic={}&rows=3{}",
            self.endpoints.crossref,
            encode_query(&query),
            self.mailto_query("&")
        );
        if let Some(body) = self.get("crossref", &url)? {
            for work in crossref::parse_search(&body)? {
                if normalize_title(&work.title) == wanted {
                    return Ok(Some(work));
                }
            }
        }
        let url = format!(
            "{}/works?search={}&per_page=3",
            self.endpoints.openalex,
            encode_query(title)
        );
        if let Some(body) = self.get("openalex", &url)? {
            for work in openalex::parse_search(&body)? {
                if normalize_title(&work.title) == wanted {
                    return Ok(Some(work));
                }
            }
        }
        Ok(None)
    }

    /// An arXiv answer names the published version's DOI when there is
    /// one; Crossref then gives the venue.
    fn complete_from_doi(&self, mut work: WorkMetadata) -> WorkMetadata {
        let Some(doi) = work.doi.clone() else {
            return work;
        };
        if let Ok(Some(published)) = self.doi(&doi) {
            if !published.publication.is_empty() {
                work.publication = published.publication;
            }
            if !published.publisher.is_empty() {
                work.publisher = published.publisher;
            }
            if published.year.is_some() {
                work.year = published.year;
            }
        }
        work
    }

    fn mailto_query(&self, separator: &str) -> String {
        match &self.mailto {
            Some(address) => format!("{separator}mailto={}", encode_query(address)),
            None => String::new(),
        }
    }

    /// GETs `url`, keeping to the service's pace and retrying when it says
    /// it is busy. `None` for a work it does not have (404).
    fn get(&self, service: &'static str, url: &str) -> Result<Option<String>> {
        let mut last_error = None;
        for attempt in 0..self.attempts {
            self.wait_turn(service);
            let response = match self.http.get(url).send() {
                Ok(response) => response,
                Err(error) => {
                    last_error = Some(anyhow!(error).context(format!("{service} did not answer")));
                    thread::sleep(backoff(attempt));
                    continue;
                }
            };
            let status = response.status();
            if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
                return Ok(None);
            }
            if status.is_success() {
                return response
                    .text()
                    .map(Some)
                    .with_context(|| format!("failed to read {service}'s answer"));
            }
            if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                let wait = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(glyphst_core::retry_after_duration)
                    .unwrap_or_else(|| backoff(attempt))
                    .min(Duration::from_secs(30));
                last_error = Some(anyhow!("{service} answered {status}"));
                thread::sleep(wait);
                continue;
            }
            return Err(anyhow!("{service} answered {status}"));
        }
        Err(last_error.unwrap_or_else(|| anyhow!("{service} did not answer")))
    }

    fn wait_turn(&self, service: &'static str) {
        // arXiv asks for one request every three seconds.
        let spacing = if service == "arxiv" {
            Duration::from_secs(3)
        } else {
            Duration::from_millis(100)
        };
        let wait = {
            let mut last = self
                .last_request
                .lock()
                .expect("metadata pacing lock poisoned");
            let now = Instant::now();
            let next = last
                .get(service)
                .map(|previous| *previous + spacing)
                .unwrap_or(now);
            last.insert(service, next.max(now));
            next.saturating_duration_since(now)
        };
        if !wait.is_zero() && !cfg!(test) {
            thread::sleep(wait);
        }
    }
}

fn backoff(attempt: u32) -> Duration {
    if cfg!(test) {
        Duration::ZERO
    } else {
        Duration::from_millis(500 * 2_u64.pow(attempt))
    }
}

fn encode_query(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            b' ' => encoded.push('+'),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// The text of a JATS or HTML fragment (Crossref abstracts are JATS).
fn strip_markup(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                plain.push(' ');
            }
            c if !in_tag => plain.push(c),
            _ => {}
        }
    }
    let plain = plain
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    let plain = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    plain
        .strip_prefix("Abstract ")
        .map(str::to_string)
        .unwrap_or(plain)
}

#[cfg(test)]
mod tests;

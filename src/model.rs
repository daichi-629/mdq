use schemars::JsonSchema;
use serde::Serialize;

#[derive(Debug)]
pub struct ParsedNote {
    pub path: String,
    pub title: String,
    pub body: String,
    pub body_start_line: usize,
    pub frontmatter: Option<serde_json::Value>,
    pub mtime: i64,
    pub ctime: i64,
    pub size: u64,
    pub hash: String,
    pub chunks: Vec<ParsedChunk>,
    pub links: Vec<ParsedLink>,
}

#[derive(Debug)]
pub struct ParsedChunk {
    pub ordinal: usize,
    pub heading: Option<String>,
    pub body: String,
}

#[derive(Debug)]
pub struct ParsedLink {
    pub raw_target: String,
    pub target: String,
    pub heading: Option<String>,
    pub is_embed: bool,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct SearchHit {
    #[serde(skip)]
    #[schemars(skip)]
    pub chunk_id: i64,
    pub path: String,
    pub title: String,
    pub heading: Option<String>,
    pub score: f64,
    pub snippet: String,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct NoteRef {
    pub path: String,
    pub title: String,
}

#[derive(Clone, Debug)]
pub struct NoteResolution {
    pub note: NoteRef,
    pub used_fallback: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GraphOutput {
    pub direction: String,
    /// Maximum hops, or null for an unlimited traversal.
    pub depth: Option<usize>,
    pub starts: Vec<NoteRef>,
    pub notes: Vec<NoteRef>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PageRecord {
    pub path: String,
    pub title: String,
    pub body: String,
    pub body_start_line: usize,
    pub metadata: serde_json::Value,
    pub mtime: i64,
    pub ctime: i64,
    pub size: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LinkRef {
    pub source: NoteRef,
    pub target: Option<NoteRef>,
    pub resolved_path: Option<String>,
    pub raw_target: String,
    pub heading: Option<String>,
    pub embed: bool,
}

#[derive(Debug)]
pub struct EmbeddingInput {
    pub content_hash: String,
    pub text: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ContextItem {
    pub path: String,
    pub heading: Option<String>,
    pub score: f64,
    pub text: String,
    pub match_reasons: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CompactContextItem<'a> {
    pub path: &'a str,
    pub heading: Option<&'a str>,
    pub text: &'a str,
}

impl<'a> From<&'a ContextItem> for CompactContextItem<'a> {
    fn from(item: &'a ContextItem) -> Self {
        Self {
            path: &item.path,
            heading: item.heading.as_deref(),
            text: &item.text,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_context_omits_ranking_details() {
        let item = ContextItem {
            path: "Notes/example.md".to_owned(),
            heading: Some("Result".to_owned()),
            score: 0.987654,
            text: "Relevant content".to_owned(),
            match_reasons: vec!["exact phrase".to_owned()],
        };

        let value = serde_json::to_value(CompactContextItem::from(&item)).unwrap();

        assert_eq!(value["path"], "Notes/example.md");
        assert_eq!(value["heading"], "Result");
        assert_eq!(value["text"], "Relevant content");
        assert!(value.get("score").is_none());
        assert!(value.get("match_reasons").is_none());
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct IndexOutput {
    pub vault: String,
    pub database: String,
    pub notes: Option<usize>,
    pub chunks: Option<usize>,
    pub links: Option<usize>,
    pub embedded: Option<usize>,
    pub model: Option<&'static str>,
}

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

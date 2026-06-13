use serde::Serialize;

#[derive(Debug)]
pub struct ParsedNote {
    pub path: String,
    pub title: String,
    pub body: String,
    pub frontmatter: Option<serde_json::Value>,
    pub mtime: i64,
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

#[derive(Clone, Debug, Serialize)]
pub struct SearchHit {
    #[serde(skip)]
    pub chunk_id: i64,
    pub path: String,
    pub title: String,
    pub heading: Option<String>,
    pub score: f64,
    pub snippet: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct NoteRef {
    pub path: String,
    pub title: String,
}

#[derive(Debug, Serialize)]
pub struct LinkRef {
    pub source: NoteRef,
    pub target: Option<NoteRef>,
    pub raw_target: String,
    pub heading: Option<String>,
    pub embed: bool,
}

#[derive(Debug)]
pub struct EmbeddingInput {
    pub content_hash: String,
    pub text: String,
}

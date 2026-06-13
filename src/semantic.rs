use anyhow::{Context, Result};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

use crate::db::Database;
use crate::model::SearchHit;

pub const MODEL_ID: &str = "multilingual-e5-small";

pub fn embed_missing(database: &mut Database, batch_size: usize) -> Result<usize> {
    let missing = database.missing_embeddings(MODEL_ID)?;
    if missing.is_empty() {
        return Ok(0);
    }

    let mut model = load_model()?;
    let mut embedded = 0;
    for batch in missing.chunks(batch_size.max(1)) {
        let documents: Vec<String> = batch.iter().map(|item| item.text.clone()).collect();
        let vectors = model
            .embed(documents, Some(batch_size.max(1)))
            .context("embedding inference failed")?;
        let items: Vec<(String, Vec<f32>)> = batch
            .iter()
            .zip(vectors)
            .map(|(input, vector)| (input.content_hash.clone(), vector))
            .collect();
        database.store_embeddings(MODEL_ID, &items)?;
        embedded += items.len();
        eprintln!("embedded {embedded}/{}", missing.len());
    }
    Ok(embedded)
}

pub fn search(database: &Database, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
    let mut model = load_model()?;
    let query = format!("query: {query}");
    let mut vectors = model
        .embed(vec![query], Some(1))
        .context("query embedding failed")?;
    let vector = vectors
        .pop()
        .context("embedding model returned no vector")?;
    database.semantic_search(MODEL_ID, &vector, limit)
}

fn load_model() -> Result<TextEmbedding> {
    let cache_dir = dirs::cache_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".cache")))
        .context("cannot determine model cache directory")?
        .join("mdq")
        .join("models");
    std::fs::create_dir_all(&cache_dir).context("cannot create model cache directory")?;
    TextEmbedding::try_new(
        InitOptions::new(EmbeddingModel::MultilingualE5Small)
            .with_cache_dir(cache_dir)
            .with_show_download_progress(true)
            .with_intra_threads(4),
    )
    .context("failed to load multilingual-e5-small")
}

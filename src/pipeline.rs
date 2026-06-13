use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, bail};

use crate::db::Database;
use crate::model::SearchHit;
use crate::query::{NativeQueryLanguage, QueryLanguage};
use crate::semantic;

pub struct PipelineEngine {
    query_languages: HashMap<&'static str, Box<dyn QueryLanguage>>,
    stage_executors: HashMap<&'static str, Box<dyn StageExecutor>>,
}

/// Shared state available to independently implemented pipeline stages.
pub struct StageContext<'a> {
    pub database: &'a Database,
    pub query_languages: &'a HashMap<&'static str, Box<dyn QueryLanguage>>,
}

/// Extension API for retrieval, filtering, reranking, or projection stages.
pub trait StageExecutor: Send + Sync {
    fn name(&self) -> &'static str;
    fn execute(
        &self,
        context: &StageContext<'_>,
        input: Vec<SearchHit>,
        argument: &str,
    ) -> Result<Vec<SearchHit>>;
}

#[derive(Debug, Clone)]
pub struct StageSpec {
    pub name: String,
    pub language: Option<String>,
    pub argument: String,
}

impl StageSpec {
    pub fn parse(source: &str) -> Result<Self> {
        let (head, argument) = source
            .split_once(':')
            .with_context(|| format!("stage must be NAME:ARGUMENT: {source}"))?;
        let (name, language) = head
            .split_once('@')
            .map(|(name, language)| (name, Some(language.to_owned())))
            .unwrap_or((head, None));
        if name.trim().is_empty() || argument.trim().is_empty() {
            bail!("stage name and argument cannot be empty: {source}");
        }
        Ok(Self {
            name: name.trim().to_ascii_lowercase(),
            language,
            argument: argument.trim().to_owned(),
        })
    }
}

impl PipelineEngine {
    pub fn standard() -> Self {
        let mut engine = Self {
            query_languages: HashMap::new(),
            stage_executors: HashMap::new(),
        };
        engine.register_query_language(Box::new(NativeQueryLanguage));
        engine.register_stage(Box::new(FilterStage));
        engine.register_stage(Box::new(Bm25Stage));
        engine.register_stage(Box::new(SemanticStage));
        engine.register_stage(Box::new(HybridStage));
        engine
    }

    pub fn register_query_language(&mut self, language: Box<dyn QueryLanguage>) {
        self.query_languages.insert(language.name(), language);
    }

    pub fn register_stage(&mut self, stage: Box<dyn StageExecutor>) {
        self.stage_executors.insert(stage.name(), stage);
    }

    pub fn execute(&self, database: &Database, stages: &[StageSpec]) -> Result<Vec<SearchHit>> {
        let context = StageContext {
            database,
            query_languages: &self.query_languages,
        };
        let mut candidates = database.all_chunks()?;
        for stage in stages {
            let executor = self
                .stage_executors
                .get(stage.name.as_str())
                .with_context(|| format!("unknown pipeline stage: {}", stage.name))?;
            if stage.language.is_some() && stage.name != "filter" {
                bail!("only filter stages accept a query language");
            }
            candidates = executor.execute(&context, candidates, &stage_argument(stage))?;
        }
        Ok(candidates)
    }
}

fn stage_argument(stage: &StageSpec) -> String {
    match &stage.language {
        Some(language) => format!("{language}\n{}", stage.argument),
        None => stage.argument.clone(),
    }
}

struct FilterStage;

impl StageExecutor for FilterStage {
    fn name(&self) -> &'static str {
        "filter"
    }

    fn execute(
        &self,
        context: &StageContext<'_>,
        input: Vec<SearchHit>,
        argument: &str,
    ) -> Result<Vec<SearchHit>> {
        let (language_name, source) = argument.split_once('\n').unwrap_or(("native", argument));
        let language = context
            .query_languages
            .get(language_name)
            .with_context(|| format!("unknown query language: {language_name}"))?;
        let expression = language.parse(source)?;
        let paths: HashSet<String> = context
            .database
            .query_frontmatter(expression.as_ref())?
            .into_iter()
            .map(|note| note.path)
            .collect();
        Ok(input
            .into_iter()
            .filter(|candidate| paths.contains(&candidate.path))
            .collect())
    }
}

struct Bm25Stage;

impl StageExecutor for Bm25Stage {
    fn name(&self) -> &'static str {
        "bm25"
    }

    fn execute(
        &self,
        context: &StageContext<'_>,
        input: Vec<SearchHit>,
        argument: &str,
    ) -> Result<Vec<SearchHit>> {
        restrict_to_input(context.database.search(argument, i64::MAX as usize)?, input)
    }
}

struct SemanticStage;

impl StageExecutor for SemanticStage {
    fn name(&self) -> &'static str {
        "rag"
    }

    fn execute(
        &self,
        context: &StageContext<'_>,
        input: Vec<SearchHit>,
        argument: &str,
    ) -> Result<Vec<SearchHit>> {
        require_embeddings(context.database)?;
        restrict_to_input(
            semantic::search(context.database, argument, usize::MAX)?,
            input,
        )
    }
}

struct HybridStage;

impl StageExecutor for HybridStage {
    fn name(&self) -> &'static str {
        "bm25+rag"
    }

    fn execute(
        &self,
        context: &StageContext<'_>,
        input: Vec<SearchHit>,
        argument: &str,
    ) -> Result<Vec<SearchHit>> {
        require_embeddings(context.database)?;
        let lexical = restrict_to_input(
            context.database.search(argument, i64::MAX as usize)?,
            input.clone(),
        )?;
        let semantic = restrict_to_input(
            semantic::search(context.database, argument, usize::MAX)?,
            input,
        )?;
        Ok(reciprocal_rank_fusion([lexical, semantic]))
    }
}

fn restrict_to_input(ranked: Vec<SearchHit>, input: Vec<SearchHit>) -> Result<Vec<SearchHit>> {
    let allowed: HashSet<i64> = input.into_iter().map(|hit| hit.chunk_id).collect();
    Ok(ranked
        .into_iter()
        .filter(|hit| allowed.contains(&hit.chunk_id))
        .collect())
}

fn reciprocal_rank_fusion<const N: usize>(rankings: [Vec<SearchHit>; N]) -> Vec<SearchHit> {
    const K: f64 = 60.0;
    let mut fused = HashMap::<i64, (SearchHit, f64)>::new();
    for ranking in rankings {
        for (rank, hit) in ranking.into_iter().enumerate() {
            let score = 1.0 / (K + rank as f64 + 1.0);
            fused
                .entry(hit.chunk_id)
                .and_modify(|(_, total)| *total += score)
                .or_insert((hit, score));
        }
    }
    let mut hits: Vec<SearchHit> = fused
        .into_values()
        .map(|(mut hit, score)| {
            hit.score = score;
            hit
        })
        .collect();
    hits.sort_by(|left, right| right.score.total_cmp(&left.score));
    hits
}

fn require_embeddings(database: &Database) -> Result<()> {
    if database.status()?.embeddings == 0 {
        bail!("no embeddings found; run `mdq --vault <path> embed` first");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parses_stage_and_query_language() {
        let stage = StageSpec::parse("filter@dataview:status = active").unwrap();
        assert_eq!(stage.name, "filter");
        assert_eq!(stage.language.as_deref(), Some("dataview"));
        assert_eq!(stage.argument, "status = active");
    }

    #[test]
    fn runs_ordered_filter_and_bm25_stages() {
        let directory = tempfile::tempdir().unwrap();
        let vault = directory.path().join("notes");
        fs::create_dir_all(&vault).unwrap();
        fs::write(
            vault.join("included.md"),
            "---\nkind: include\n---\n# Included\nshared phrase\n",
        )
        .unwrap();
        fs::write(
            vault.join("excluded.md"),
            "---\nkind: exclude\n---\n# Excluded\nshared phrase shared phrase\n",
        )
        .unwrap();
        let mut database = Database::open(&directory.path().join("index.sqlite3")).unwrap();
        database.rebuild(&vault).unwrap();
        let engine = PipelineEngine::standard();

        let filter_then_rank = engine
            .execute(
                &database,
                &[
                    StageSpec::parse("filter:kind = include").unwrap(),
                    StageSpec::parse("bm25:shared phrase").unwrap(),
                ],
            )
            .unwrap();
        let rank_then_filter = engine
            .execute(
                &database,
                &[
                    StageSpec::parse("bm25:shared phrase").unwrap(),
                    StageSpec::parse("filter:kind = include").unwrap(),
                ],
            )
            .unwrap();

        assert_eq!(filter_then_rank.len(), 1);
        assert_eq!(rank_then_filter.len(), 1);
        assert_eq!(filter_then_rank[0].path, "included.md");
        assert_eq!(rank_then_filter[0].path, "included.md");
    }
}

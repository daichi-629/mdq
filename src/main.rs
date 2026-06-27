use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use mdq::compat::CompatibilityEngine;
use mdq::core::{QueryContext, RecordSet};
use mdq::db::{Database, default_db_path};
use mdq::manual;
use mdq::model::{NoteRef, SearchHit};
use mdq::pipeline::{PipelineEngine, StageSpec};
use mdq::semantic;
use serde::Serialize;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Root directory of the Markdown collection.
    #[arg(long, global = true, default_value = ".")]
    vault: PathBuf,

    /// Override the generated SQLite index path.
    #[arg(long, global = true)]
    db: Option<PathBuf>,

    /// Emit machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,

    /// Maximum number of changed files/chunks to refresh automatically before
    /// running a search command. Larger drift requires an explicit `index` run.
    #[arg(long, global = true, default_value_t = 20)]
    auto_threshold: usize,

    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum BuildTarget {
    /// BM25 full-text index only.
    Bm25,
    /// Local semantic embeddings only.
    Embed,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SearchEngine {
    /// BM25 full-text retrieval only.
    Bm25,
    /// Semantic embedding retrieval only.
    Rag,
}

#[derive(Subcommand)]
enum Command {
    /// Build the BM25 index and local semantic embeddings.
    Index {
        /// Build only this target instead of both.
        #[arg(long, value_enum)]
        only: Option<BuildTarget>,
        #[arg(long, default_value_t = 64)]
        batch_size: usize,
    },
    /// Retrieve source context for a query (default: hybrid BM25 + semantic).
    Search {
        query: String,
        #[arg(short, long, default_value_t = 8)]
        limit: usize,
        #[arg(long, default_value_t = 2000)]
        max_chars: usize,
        /// Use only one retrieval engine instead of the hybrid default.
        #[arg(long, value_enum)]
        only: Option<SearchEngine>,
        /// Include score and heading detail in the output.
        #[arg(short, long)]
        verbose: bool,
    },
    /// Run a native, Tasks, Base, Dataview, or DataviewJS query.
    Query {
        /// Inline query source. Use --file for .base files or longer scripts.
        expression: Option<String>,
        #[arg(long, default_value = "native")]
        language: String,
        #[arg(long)]
        file: Option<PathBuf>,
        /// Current note used by this.file and dv.current().
        #[arg(long)]
        current: Option<PathBuf>,
        #[arg(short, long, default_value_t = 100)]
        limit: usize,
    },
    /// List notes linking to a note.
    Backlinks { note: String },
    /// List links from a note.
    Links { note: String },
    /// Traverse resolved links in both directions.
    Graph {
        note: String,
        #[arg(long, default_value_t = 2)]
        depth: usize,
    },
    /// Run filters and rankers in the exact order supplied.
    Pipeline {
        /// Stage syntax: filter[@language]:EXPR, bm25:QUERY, rag:QUERY, bm25+rag:QUERY
        #[arg(long = "stage", required = true)]
        stages: Vec<String>,
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
        /// Return full chunk context instead of search snippets.
        #[arg(long)]
        context: bool,
        #[arg(long, default_value_t = 12000)]
        max_chars: usize,
        /// Include score and heading detail in the output.
        #[arg(short, long)]
        verbose: bool,
    },
    /// Show index metadata and counts.
    Status,
    /// Show the per-command manual, query language reference, and examples.
    #[command(alias = "man")]
    Manual {
        /// Topic: overview, index, search, query, backlinks, links, graph,
        /// pipeline, status, native, tasks, base, dataview, dataviewjs,
        /// extensions, examples, all.
        topic: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Command::Manual { topic } = &cli.command {
        let rendered = manual::render(topic.as_deref())?;
        print!("{rendered}");
        if !rendered.ends_with('\n') {
            println!();
        }
        return Ok(());
    }
    let vault = cli
        .vault
        .canonicalize()
        .with_context(|| format!("vault does not exist: {}", cli.vault.display()))?;
    let db_path = cli.db.unwrap_or(default_db_path(&vault)?);
    let mut database = Database::open(&db_path)?;
    let pipeline = PipelineEngine::standard();
    let compatibility = CompatibilityEngine::standard();

    let needs_fresh_index = !matches!(
        cli.command,
        Command::Index { .. } | Command::Status | Command::Manual { .. }
    );
    if needs_fresh_index {
        ensure_index_fresh(&mut database, &vault, cli.auto_threshold)?;
    }

    match cli.command {
        Command::Index { only, batch_size } => {
            let build_bm25 = only != Some(BuildTarget::Embed);
            let build_embed = only != Some(BuildTarget::Bm25);
            let stats = build_bm25.then(|| database.rebuild(&vault)).transpose()?;
            let embedded = build_embed
                .then(|| semantic::embed_missing(&mut database, batch_size))
                .transpose()?;
            if cli.json {
                print_json(&serde_json::json!({
                    "vault": vault,
                    "database": db_path,
                    "notes": stats.as_ref().map(|stats| stats.notes),
                    "chunks": stats.as_ref().map(|stats| stats.chunks),
                    "links": stats.as_ref().map(|stats| stats.links),
                    "embedded": embedded,
                    "model": build_embed.then_some(semantic::MODEL_ID),
                }))?;
            } else {
                if let Some(stats) = &stats {
                    println!(
                        "indexed {} notes, {} chunks, {} links",
                        stats.notes, stats.chunks, stats.links
                    );
                }
                if let Some(embedded) = embedded {
                    println!("embedded {embedded} chunks with {}", semantic::MODEL_ID);
                }
                println!("{}", db_path.display());
            }
        }
        Command::Search {
            query,
            limit,
            max_chars,
            only,
            verbose,
        } => {
            if only != Some(SearchEngine::Bm25) {
                ensure_embeddings_fresh(&mut database, &vault, cli.auto_threshold)?;
            }
            let (stage, fetch_limit) = match only {
                Some(SearchEngine::Bm25) => ("bm25", limit.saturating_mul(3).max(limit)),
                Some(SearchEngine::Rag) => ("rag", limit.saturating_mul(3).max(limit)),
                None => ("bm25+rag", limit.saturating_mul(5).max(30)),
            };
            let hits = run_alias(&pipeline, &database, stage, &query, fetch_limit)?;
            let context = build_context(&database, hits, limit, max_chars)?;
            output_context(context, cli.json, verbose)?;
        }
        Command::Query {
            expression,
            language,
            file,
            current,
            limit,
        } => {
            let source = match (expression, file) {
                (Some(source), None) => source,
                (None, Some(path)) => std::fs::read_to_string(&path)
                    .with_context(|| format!("cannot read query file {}", path.display()))?,
                (Some(_), Some(_)) => {
                    bail!("provide either inline query source or --file, not both")
                }
                (None, None) => bail!("query source is required"),
            };
            if language == "native" {
                if source.trim().is_empty() {
                    bail!("query expression cannot be empty");
                }
                let hits = run_alias(&pipeline, &database, "filter", &source, usize::MAX)?;
                let notes = unique_notes(&hits, limit);
                output_notes(&notes, cli.json)?;
            } else {
                let current_file = current.map(|path| {
                    if path.is_absolute() {
                        path
                    } else {
                        vault.join(path)
                    }
                });
                let context = QueryContext {
                    database: &database,
                    vault: &vault,
                    current_file,
                };
                let mut result = compatibility.execute(&language, &context, &source)?;
                result.rows.truncate(limit);
                output_record_set(&result, cli.json)?;
            }
        }
        Command::Backlinks { note } => {
            let links = database.backlinks(&note)?;
            if cli.json {
                print_json(&links)?;
            } else {
                for link in links {
                    println!("{}\t{}", link.source.path, link.raw_target);
                }
            }
        }
        Command::Links { note } => {
            let links = database.outgoing_links(&note)?;
            if cli.json {
                print_json(&links)?;
            } else {
                for link in links {
                    let resolved = link
                        .target
                        .map(|target| target.path)
                        .unwrap_or_else(|| "<unresolved>".to_owned());
                    println!("{}\t{}\t{}", link.raw_target, resolved, link.embed);
                }
            }
        }
        Command::Graph { note, depth } => {
            let graph = traverse_graph(&database, &note, depth)?;
            output_notes(&graph, cli.json)?;
        }
        Command::Pipeline {
            stages,
            limit,
            context,
            max_chars,
            verbose,
        } => {
            let stages = stages
                .iter()
                .map(|stage| StageSpec::parse(stage))
                .collect::<Result<Vec<_>>>()?;
            if stages
                .iter()
                .any(|stage| stage.name == "rag" || stage.name == "bm25+rag")
            {
                ensure_embeddings_fresh(&mut database, &vault, cli.auto_threshold)?;
            }
            let mut hits = pipeline.execute(&database, &stages)?;
            if context {
                let context = build_context(&database, hits, limit, max_chars)?;
                output_context(context, cli.json, verbose)?;
            } else {
                hits.truncate(limit);
                output_hits(&hits, cli.json, verbose)?;
            }
        }
        Command::Status => {
            let status = database.status(semantic::MODEL_ID)?;
            if cli.json {
                print_json(&status)?;
            } else {
                println!(
                    "vault: {}",
                    status.vault.as_deref().unwrap_or("<not indexed>")
                );
                println!(
                    "indexed_at: {}",
                    status.indexed_at.as_deref().unwrap_or("-")
                );
                println!("notes: {}", status.notes);
                println!("chunks: {}", status.chunks);
                println!("links: {}", status.links);
                println!("unresolved_links: {}", status.unresolved_links);
                println!("embeddings: {}", status.embeddings);
                println!("cached_embeddings: {}", status.cached_embeddings);
                println!("index_stale: {}", status.index_stale);
                println!("embeddings_stale: {}", status.embeddings_stale);
                println!("database: {}", db_path.display());
            }
        }
        Command::Manual { .. } => unreachable!("manual exits before database initialization"),
    }
    Ok(())
}

/// Refreshes the BM25 index when the vault has drifted from it. A small drift on an
/// already-indexed vault is refreshed automatically; a large or first-time drift requires
/// an explicit `index` run, since that may mean the wrong vault or an unbuilt index.
fn ensure_index_fresh(database: &mut Database, vault: &Path, threshold: usize) -> Result<()> {
    let changed = database.staleness(vault)?;
    if changed == 0 {
        return Ok(());
    }
    if database.has_index()? && changed <= threshold {
        eprintln!("vault changed ({changed} file(s)); refreshing index automatically");
        database.rebuild(vault)?;
        Ok(())
    } else {
        bail!(
            "index is stale ({changed} changed file(s)); run `mdq --vault {} index` to refresh",
            vault.display()
        )
    }
}

/// Refreshes embeddings when chunks are missing them, under the same threshold policy as
/// `ensure_index_fresh`.
fn ensure_embeddings_fresh(database: &mut Database, vault: &Path, threshold: usize) -> Result<()> {
    let missing = database.missing_embeddings_count(semantic::MODEL_ID)?;
    if missing == 0 {
        return Ok(());
    }
    if database.has_embeddings()? && missing <= threshold {
        eprintln!("{missing} chunk(s) missing embeddings; embedding automatically");
        semantic::embed_missing(database, 64)?;
        Ok(())
    } else {
        bail!(
            "embeddings are missing or stale ({missing} chunk(s)); run `mdq --vault {} index` to refresh",
            vault.display()
        )
    }
}

fn output_hits(hits: &[SearchHit], json: bool, verbose: bool) -> Result<()> {
    if json {
        return print_json(hits);
    }
    for hit in hits {
        let heading = hit
            .heading
            .as_deref()
            .map(|heading| format!("#{heading}"))
            .unwrap_or_default();
        if verbose {
            println!("{}{} (score={:.6})", hit.path, heading, hit.score);
        } else {
            println!("{}{}", hit.path, heading);
        }
        println!("{}", hit.snippet);
    }
    Ok(())
}

fn output_notes(notes: &[NoteRef], json: bool) -> Result<()> {
    if json {
        return print_json(notes);
    }
    for note in notes {
        println!("{}", note.path);
    }
    Ok(())
}

fn output_record_set(result: &RecordSet, json: bool) -> Result<()> {
    if json {
        return print_json(result);
    }
    for row in &result.rows {
        println!("{}", serde_json::to_string(row)?);
    }
    for diagnostic in &result.diagnostics {
        eprintln!("warning: {diagnostic}");
    }
    Ok(())
}

fn unique_notes(hits: &[SearchHit], limit: usize) -> Vec<NoteRef> {
    let mut seen = HashSet::new();
    hits.iter()
        .filter(|hit| seen.insert(hit.path.clone()))
        .take(limit)
        .map(|hit| NoteRef {
            path: hit.path.clone(),
            title: hit.title.clone(),
        })
        .collect()
}

fn traverse_graph(database: &Database, start: &str, depth: usize) -> Result<Vec<NoteRef>> {
    let Some((start_note, _)) = database.note_body(start)? else {
        bail!("note not found or ambiguous: {start}");
    };
    let mut queue = VecDeque::from([(start_note.clone(), 0)]);
    let mut seen = HashSet::from([start_note.path.clone()]);
    let mut result = vec![start_note];

    while let Some((note, current_depth)) = queue.pop_front() {
        if current_depth >= depth {
            continue;
        }
        let mut neighbors = Vec::new();
        for link in database.outgoing_links(&note.path)? {
            if let Some(target) = link.target {
                neighbors.push(target);
            }
        }
        for link in database.backlinks(&note.path)? {
            neighbors.push(link.source);
        }
        for neighbor in neighbors {
            if seen.insert(neighbor.path.clone()) {
                queue.push_back((neighbor.clone(), current_depth + 1));
                result.push(neighbor);
            }
        }
    }
    Ok(result)
}

#[derive(Serialize)]
struct ContextItem {
    path: String,
    heading: Option<String>,
    score: f64,
    text: String,
}

fn build_context(
    database: &Database,
    hits: Vec<SearchHit>,
    limit: usize,
    max_chars: usize,
) -> Result<Vec<ContextItem>> {
    let mut best_by_path = HashMap::<String, SearchHit>::new();
    for hit in hits {
        best_by_path
            .entry(hit.path.clone())
            .and_modify(|existing| {
                if hit.score > existing.score {
                    *existing = hit.clone();
                }
            })
            .or_insert(hit);
    }
    let mut hits: Vec<SearchHit> = best_by_path.into_values().collect();
    hits.sort_by(|left, right| right.score.total_cmp(&left.score));

    let mut used = 0;
    let mut context = Vec::new();
    for hit in hits.into_iter().take(limit) {
        let Some(body) = database.chunk_body(hit.chunk_id)? else {
            continue;
        };
        let remaining = max_chars.saturating_sub(used);
        if remaining == 0 {
            break;
        }
        let header_len = hit.path.chars().count()
            + hit.heading.as_deref().map(|h| 1 + h.chars().count()).unwrap_or(0);
        let text: String = body.chars().take(remaining.saturating_sub(header_len)).collect();
        used += header_len + text.chars().count();
        context.push(ContextItem {
            path: hit.path,
            heading: hit.heading,
            score: hit.score,
            text,
        });
    }
    Ok(context)
}

fn run_alias(
    pipeline: &PipelineEngine,
    database: &Database,
    stage: &str,
    argument: &str,
    limit: usize,
) -> Result<Vec<SearchHit>> {
    let spec = StageSpec::parse(&format!("{stage}:{argument}"))?;
    let mut hits = pipeline.execute(database, &[spec])?;
    hits.truncate(limit);
    Ok(hits)
}

fn output_context(context: Vec<ContextItem>, json: bool, verbose: bool) -> Result<()> {
    if json {
        return print_json(&context);
    }
    for (index, item) in context.iter().enumerate() {
        if index > 0 {
            println!();
        }
        let heading = item
            .heading
            .as_deref()
            .map(|heading| format!("#{heading}"))
            .unwrap_or_default();
        if verbose {
            println!("{}{} (score={:.6})", item.path, heading, item.score);
        } else {
            println!("{}{}", item.path, heading);
        }
        println!("{}", item.text);
    }
    Ok(())
}

fn print_json<T: Serialize + ?Sized>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

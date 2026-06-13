use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use mdq::db::{Database, default_db_path};
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

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build a fresh local index.
    Index,
    /// Search indexed Markdown chunks with BM25.
    Search {
        query: String,
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
    /// Generate and cache local multilingual embeddings.
    Embed {
        #[arg(long, default_value_t = 64)]
        batch_size: usize,
    },
    /// Search by semantic similarity using local embeddings.
    Semantic {
        query: String,
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
    /// Query arbitrary YAML frontmatter paths.
    Query {
        expression: String,
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
    /// Retrieve compact source context for an LLM or RAG pipeline.
    Context {
        query: String,
        #[arg(short, long, default_value_t = 8)]
        limit: usize,
        #[arg(long, default_value_t = 12000)]
        max_chars: usize,
    },
    /// Retrieve hybrid BM25 + semantic context for a RAG pipeline.
    Rag {
        query: String,
        #[arg(short, long, default_value_t = 8)]
        limit: usize,
        #[arg(long, default_value_t = 12000)]
        max_chars: usize,
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
    },
    /// Show index metadata and counts.
    Status,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let vault = cli
        .vault
        .canonicalize()
        .with_context(|| format!("vault does not exist: {}", cli.vault.display()))?;
    let db_path = cli.db.unwrap_or(default_db_path(&vault)?);
    let mut database = Database::open(&db_path)?;
    let pipeline = PipelineEngine::standard();

    match cli.command {
        Command::Index => {
            let stats = database.rebuild(&vault)?;
            if cli.json {
                print_json(&serde_json::json!({
                    "vault": vault,
                    "database": db_path,
                    "notes": stats.notes,
                    "chunks": stats.chunks,
                    "links": stats.links,
                }))?;
            } else {
                println!(
                    "indexed {} notes, {} chunks, {} links\n{}",
                    stats.notes,
                    stats.chunks,
                    stats.links,
                    db_path.display()
                );
            }
        }
        Command::Search { query, limit } => {
            let hits = run_alias(&pipeline, &database, "bm25", &query, limit)?;
            output_hits(&hits, cli.json)?;
        }
        Command::Embed { batch_size } => {
            let count = semantic::embed_missing(&mut database, batch_size)?;
            if cli.json {
                print_json(&serde_json::json!({
                    "model": semantic::MODEL_ID,
                    "embedded": count,
                }))?;
            } else {
                println!("embedded {count} chunks with {}", semantic::MODEL_ID);
            }
        }
        Command::Semantic { query, limit } => {
            let hits = run_alias(&pipeline, &database, "rag", &query, limit)?;
            output_hits(&hits, cli.json)?;
        }
        Command::Query { expression, limit } => {
            let hits = run_alias(&pipeline, &database, "filter", &expression, usize::MAX)?;
            let notes = unique_notes(&hits, limit);
            output_notes(&notes, cli.json)?;
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
        Command::Context {
            query,
            limit,
            max_chars,
        } => {
            let hits = run_alias(
                &pipeline,
                &database,
                "bm25",
                &query,
                limit.saturating_mul(3).max(limit),
            )?;
            let context = build_context(&database, hits, limit, max_chars)?;
            output_context(context, cli.json)?;
        }
        Command::Rag {
            query,
            limit,
            max_chars,
        } => {
            let hits = run_alias(
                &pipeline,
                &database,
                "bm25+rag",
                &query,
                limit.saturating_mul(5).max(30),
            )?;
            let context = build_context(&database, hits, limit, max_chars)?;
            output_context(context, cli.json)?;
        }
        Command::Pipeline {
            stages,
            limit,
            context,
            max_chars,
        } => {
            let stages = stages
                .iter()
                .map(|stage| StageSpec::parse(stage))
                .collect::<Result<Vec<_>>>()?;
            let mut hits = pipeline.execute(&database, &stages)?;
            if context {
                let context = build_context(&database, hits, limit, max_chars)?;
                output_context(context, cli.json)?;
            } else {
                hits.truncate(limit);
                output_hits(&hits, cli.json)?;
            }
        }
        Command::Status => {
            let status = database.status()?;
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
                println!("database: {}", db_path.display());
            }
        }
    }
    Ok(())
}

fn output_hits(hits: &[SearchHit], json: bool) -> Result<()> {
    if json {
        return print_json(hits);
    }
    for hit in hits {
        let heading = hit
            .heading
            .as_deref()
            .map(|heading| format!("#{heading}"))
            .unwrap_or_default();
        println!(
            "{:.6}\t{}{}\n  {}",
            hit.score, hit.path, heading, hit.snippet
        );
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
        let text: String = body.chars().take(remaining).collect();
        used += text.chars().count();
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

fn output_context(context: Vec<ContextItem>, json: bool) -> Result<()> {
    if json {
        return print_json(&context);
    }
    for item in context {
        println!("---\nsource: {}", item.path);
        if let Some(heading) = item.heading {
            println!("heading: {heading}");
        }
        println!("score: {:.6}\n\n{}", item.score, item.text);
    }
    Ok(())
}

fn print_json<T: Serialize + ?Sized>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

//! R0 offline evaluation of the public literal-search tool, not a code index.
//! Run with `cargo run -p ziyuan-runtime --example code_search_baseline --locked`.
//! Only synthetic files in a uniquely owned temporary directory are accessed.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};
use ziyuan_core::{Capability, Seal};
use ziyuan_runtime::{BoxError, tools::sealed_tools};

const FIXTURE: &str = include_str!("fixtures/code-search-v1.json");
const REPEATS: usize = 25;
const K: usize = 5;

struct FixtureDirectory(PathBuf);

impl Drop for FixtureDirectory {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0)
            && error.kind() != io::ErrorKind::NotFound
        {
            eprintln!("R0 fixture cleanup failed: {error}");
        }
    }
}

fn sha256(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn write_files(root: &Path, files: &Value) -> io::Result<()> {
    for (name, text) in files.as_object().expect("fixture file map") {
        let path = root.join(name);
        fs::create_dir_all(path.parent().expect("fixture parent"))?;
        fs::write(path, text.as_str().expect("fixture text"))?;
    }
    Ok(())
}

fn snapshot(root: &Path) -> io::Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = BTreeMap::new();
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            } else {
                files.insert(
                    entry
                        .path()
                        .strip_prefix(root)
                        .expect("fixture path")
                        .into(),
                    fs::read(entry.path())?,
                );
            }
        }
    }
    Ok(files)
}

fn main() -> Result<(), BoxError> {
    let fixture: Value = serde_json::from_str(FIXTURE)?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let directory = std::env::temp_dir().join(format!("ziyuan-r0-{}-{stamp}", std::process::id()));
    // Claim ownership only after exclusive creation; never clean up a collision.
    fs::create_dir(&directory)?;
    let directory = FixtureDirectory(directory);
    let mut cases = Vec::new();
    let mut recall_sum = 0.0;
    let mut precision_sum = 0.0;
    let mut positives = 0;
    let mut negatives = 0;
    let mut false_positive_queries = 0;
    for case in fixture["cases"].as_array().expect("fixture cases") {
        let root = directory.0.join(case["id"].as_str().expect("case ID"));
        write_files(&root, &fixture["files"])?;
        if let Some(writes) = case.get("writes") {
            write_files(&root, writes)?;
        }
        if let Some(renames) = case["renames"].as_object() {
            for (from, to) in renames {
                fs::rename(
                    root.join(from),
                    root.join(to.as_str().expect("rename path")),
                )?;
            }
        }
        if let Some(deletes) = case["deletes"].as_array() {
            for path in deletes {
                fs::remove_file(root.join(path.as_str().expect("delete path")))?;
            }
        }
        let before = snapshot(&root)?;
        let seal = Seal {
            capabilities: [Capability::SearchCode].into(),
            scope: serde_json::from_value(case["scope"].clone())?,
            mcp: BTreeSet::new(),
        };
        let sealed = sealed_tools(&root, &seal, None)?;
        let search = &sealed.tools[0];
        let arguments = json!({"query": case["query"]}).to_string();
        let mut elapsed = Vec::new();
        let mut output = None;
        for _ in 0..REPEATS {
            let start = Instant::now();
            let current = search.execute(&arguments)?;
            elapsed.push(start.elapsed().as_nanos());
            if let Some(previous) = &output {
                assert_eq!(previous, &current, "unstable result: {}", case["id"]);
            }
            output = Some(current);
        }
        assert_eq!(before, snapshot(&root)?, "search modified fixture files");
        assert!(sealed.changes().is_empty(), "search staged changes");
        let output = output.expect("nonempty repeat count");
        let hits: Vec<_> = output
            .lines()
            .filter_map(|line| line.split_once(": ").map(|(location, _)| location))
            .collect();
        assert_eq!(hits.iter().collect::<BTreeSet<_>>().len(), hits.len());
        for (location, snippet) in output.lines().filter_map(|line| line.split_once(": ")) {
            let (path, line) = location.rsplit_once(':').expect("hit location");
            let line: usize = line.parse()?;
            let text = fs::read_to_string(root.join(path))?;
            assert!(line > 0, "invalid hit line");
            assert_eq!(text.lines().nth(line - 1).map(str::trim), Some(snippet));
        }
        if case["required_empty"].as_bool() == Some(true) {
            assert!(hits.is_empty(), "required exclusion failed: {}", case["id"]);
        }
        let relevant: Vec<String> = serde_json::from_value(case["relevant"].clone())?;
        // Validate manual labels against disk coordinates, not search results.
        for location in &relevant {
            let (path, line) = location.rsplit_once(':').expect("label location");
            let line: usize = line.parse()?;
            let text = fs::read_to_string(root.join(path))?;
            assert!(line > 0 && line <= text.lines().count(), "invalid label");
        }
        let matched = hits
            .iter()
            .take(K)
            .filter(|location| relevant.iter().any(|expected| expected == **location))
            .count();
        if matches!(case["category"].as_str(), Some("exact" | "freshness")) {
            assert_eq!(
                matched,
                relevant.len(),
                "missed fixed location: {}",
                case["id"]
            );
            assert_eq!(
                hits.len(),
                relevant.len(),
                "unexpected location: {}",
                case["id"]
            );
        }
        let (recall, precision) = if relevant.is_empty() {
            negatives += 1;
            false_positive_queries += usize::from(!hits.is_empty());
            (None, None)
        } else {
            positives += 1;
            let recall = matched as f64 / relevant.len() as f64;
            // Divide by returned candidates (at most K), not by K padded slots.
            let precision = matched as f64 / hits.len().clamp(1, K) as f64;
            recall_sum += recall;
            precision_sum += precision;
            (Some(recall), Some(precision))
        };
        let first_ns = elapsed[0];
        elapsed.sort_unstable();
        cases.push(json!({
            "id": case["id"], "category": case["category"],
            "scope": case["scope"], "query": case["query"],
            "relevant": relevant, "hits": hits,
            "validated_locations": hits.len(),
            "recall_at_5": recall, "returned_precision_at_5": precision,
            "first_query_ns": first_ns, "p50_ns": elapsed[REPEATS / 2],
            "p95_ns": elapsed[(REPEATS * 95).div_ceil(100) - 1],
            "fixture_files": before.len(),
            "fixture_bytes": before.values().map(Vec::len).sum::<usize>()
        }));
    }
    // Clean up before reporting success, so normal cleanup errors are not hidden.
    fs::remove_dir_all(&directory.0)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "fixture": fixture["version"], "fixture_sha256": sha256(FIXTURE.as_bytes()),
            "runner_sha256": sha256(include_bytes!("code_search_baseline.rs")),
            "search_source_sha256": sha256(include_bytes!("../src/tools.rs")),
            "runtime_version": env!("CARGO_PKG_VERSION"),
            "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
            "method": "public search_code: case-sensitive literal, path/line order, not ranked",
            "repeats": REPEATS, "k": K, "positive_cases": positives,
            "negative_cases": negatives, "false_positive_queries": false_positive_queries,
            "macro_recall_at_5": recall_sum / positives as f64,
            "macro_returned_precision_at_5": precision_sum / positives as f64,
            "negative_false_positive_rate": false_positive_queries as f64 / negatives as f64,
            "index_build_time_ns": null, "index_bytes": null,
            "external_requests": 0, "cases": cases
        }))?
    );
    Ok(())
}

//! `tokensave_rename` (#568): graph-based rename with confidence classes.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use serde_json::{json, Value};
use tempfile::TempDir;
use tokensave::config::Toolset;
use tokensave::mcp::handle_tool_call;
use tokensave::mcp::tools::{get_listed_tool_definitions, get_tool_definitions, tool_area};
use tokensave::tokensave::TokenSave;

const UTILS_RS: &str = "/// Computes the total.
pub fn compute_total(x: i32) -> i32 {
    x + 1
}

pub fn other_fn() -> i32 {
    2
}
";

const MAIN_RS: &str = "mod utils;
use crate::utils::compute_total;

fn main() {
    // compute_total is called here
    let v = compute_total(1);
    let w = utils::compute_total(2);
    let s = \"compute_total\";
    println!(\"{} {} {}\", v, w, s);
}
";

async fn rust_project() -> (TempDir, TokenSave) {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/utils.rs"), UTILS_RS).unwrap();
    fs::write(root.join("src/main.rs"), MAIN_RS).unwrap();
    let cg = TokenSave::init(root).await.unwrap();
    cg.index_all().await.unwrap();
    (dir, cg)
}

async fn python_project() -> (TempDir, TokenSave) {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::write(
        root.join("app.py"),
        "class Greeter:\n    def greet(self):\n        return \"hi\"\n\n\ndef run(g):\n    return g.greet()\n",
    )
    .unwrap();
    let cg = TokenSave::init(root).await.unwrap();
    cg.index_all().await.unwrap();
    (dir, cg)
}

async fn node_id(cg: &TokenSave, name: &str) -> String {
    let nodes = cg.get_nodes_by_name(name).await.unwrap();
    nodes
        .iter()
        .find(|n| n.kind.as_str() != "use")
        .unwrap_or_else(|| panic!("no node named {name}"))
        .id
        .clone()
}

async fn call(cg: &TokenSave, tool: &str, args: Value) -> Value {
    let result = handle_tool_call(cg, tool, args, None, None).await.unwrap();
    let text = result.value["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap_or_else(|_| json!({ "raw": text }))
}

/// All non-text sites as `(file, line, confidence, kind)`.
fn sites(plan: &Value) -> Vec<(String, u64, String, String)> {
    let mut out = Vec::new();
    for file in plan["files"].as_array().unwrap() {
        for site in file["sites"].as_array().unwrap() {
            out.push((
                file["file"].as_str().unwrap().to_string(),
                site["line"].as_u64().unwrap(),
                site["confidence"].as_str().unwrap().to_string(),
                site["kind"].as_str().unwrap().to_string(),
            ));
        }
    }
    out
}

fn read(root: &Path, rel: &str) -> String {
    fs::read_to_string(root.join(rel)).unwrap()
}

#[tokio::test]
async fn dry_run_classifies_sites_and_previews_the_diff() {
    let (dir, cg) = rust_project().await;
    let id = node_id(&cg, "compute_total").await;
    let plan = call(
        &cg,
        "tokensave_rename",
        json!({"node_id": id, "new_name": "sum_total"}),
    )
    .await;

    assert_eq!(plan["dry_run"], true);
    let sites = sites(&plan);
    // Definition, the bare call, the `utils::` path call and the `use`.
    assert!(sites.contains(&(
        "src/utils.rs".into(),
        2,
        "exact".into(),
        "definition".into()
    )));
    assert!(
        sites.contains(&("src/main.rs".into(), 6, "exact".into(), "calls".into())),
        "{sites:?}"
    );
    assert!(
        sites.contains(&("src/main.rs".into(), 7, "exact".into(), "calls".into())),
        "{sites:?}"
    );
    assert!(
        sites.contains(&("src/main.rs".into(), 2, "exact".into(), "uses".into())),
        "{sites:?}"
    );
    assert_eq!(plan["counts"]["exact"], 4, "{plan:#}");
    assert_eq!(plan["counts"]["heuristic"], 0, "{plan:#}");

    // The comment and the string literal are listed, not edited.
    let text_only = plan["text_only"].as_array().unwrap();
    let contexts: BTreeSet<&str> = text_only
        .iter()
        .map(|t| t["kind"].as_str().unwrap())
        .collect();
    assert!(contexts.contains("comment"), "{text_only:?}");
    assert!(contexts.contains("string"), "{text_only:?}");

    let diff = plan["diff"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d.as_str().unwrap())
        .collect::<String>();
    assert!(diff.contains("--- a/src/main.rs"), "{diff}");
    assert!(diff.contains("+    let v = sum_total(1);"), "{diff}");
    assert!(
        diff.contains("+pub fn sum_total(x: i32) -> i32 {"),
        "{diff}"
    );
    assert!(
        !diff.contains("+    let s = \"sum_total\";"),
        "strings are never edited: {diff}"
    );

    // A dry run writes nothing.
    assert_eq!(read(dir.path(), "src/main.rs"), MAIN_RS);
    assert_eq!(read(dir.path(), "src/utils.rs"), UTILS_RS);
}

#[tokio::test]
async fn apply_edits_exact_sites_and_reindexes() {
    let (dir, cg) = rust_project().await;
    let plan = call(
        &cg,
        "tokensave_rename",
        json!({"symbol": "compute_total", "new_name": "sum_total", "dry_run": false}),
    )
    .await;
    assert_eq!(plan["applied"], true, "{plan:#}");

    let main = read(dir.path(), "src/main.rs");
    assert!(main.contains("use crate::utils::sum_total;"), "{main}");
    assert!(main.contains("let v = sum_total(1);"), "{main}");
    assert!(main.contains("let w = utils::sum_total(2);"), "{main}");
    // Comments and strings are left for a human.
    assert!(main.contains("// compute_total is called here"), "{main}");
    assert!(main.contains("\"compute_total\""), "{main}");
    assert!(read(dir.path(), "src/utils.rs").contains("pub fn sum_total(x: i32)"));

    // The edited files were reindexed.
    assert!(!cg.get_nodes_by_name("sum_total").await.unwrap().is_empty());
    assert!(cg
        .get_nodes_by_name("compute_total")
        .await
        .unwrap()
        .iter()
        .all(|n| n.kind.as_str() == "use"));
}

#[tokio::test]
async fn heuristic_sites_refuse_apply_without_allow_heuristic() {
    let (dir, cg) = python_project().await;
    let id = node_id(&cg, "greet").await;
    let original = read(dir.path(), "app.py");

    let plan = call(
        &cg,
        "tokensave_rename",
        json!({"node_id": id, "new_name": "salute"}),
    )
    .await;
    let sites = sites(&plan);
    assert!(
        sites.contains(&("app.py".into(), 2, "exact".into(), "definition".into())),
        "{sites:?}"
    );
    // `g.greet()` is bound by the trailing segment of a dotted receiver.
    assert!(
        sites.contains(&("app.py".into(), 7, "heuristic".into(), "calls".into())),
        "{sites:?}"
    );

    let refused = call(
        &cg,
        "tokensave_rename",
        json!({"node_id": id, "new_name": "salute", "dry_run": false}),
    )
    .await;
    assert_eq!(refused["applied"], false, "{refused:#}");
    assert!(refused["refused"]
        .as_str()
        .unwrap()
        .contains("allow_heuristic"));
    let blocking = refused["blocking_sites"].as_array().unwrap();
    assert!(blocking.iter().any(|s| s["line"] == 7), "{blocking:?}");
    assert_eq!(read(dir.path(), "app.py"), original, "nothing is written");

    let applied = call(
        &cg,
        "tokensave_rename",
        json!({"node_id": id, "new_name": "salute", "dry_run": false, "allow_heuristic": true}),
    )
    .await;
    assert_eq!(applied["applied"], true, "{applied:#}");
    let after = read(dir.path(), "app.py");
    assert!(after.contains("def salute(self):"), "{after}");
    assert!(after.contains("return g.salute()"), "{after}");
}

#[tokio::test]
async fn a_reserved_word_is_refused_and_nothing_is_written() {
    let (dir, cg) = rust_project().await;
    // `fn` has an identifier's shape, and tree-sitter-rust parses `fn fn()`
    // without an error node, so it must be refused up front. The
    // all-or-nothing parse check is covered by the `commit_edits` unit tests.
    let result = call(
        &cg,
        "tokensave_rename",
        json!({"symbol": "compute_total", "new_name": "fn", "dry_run": false}),
    )
    .await;
    assert_eq!(result["applied"], false, "{result:#}");
    let refused = result["refused"].as_str().unwrap();
    assert!(refused.contains("reserved word in Rust"), "{refused}");
    assert_eq!(read(dir.path(), "src/main.rs"), MAIN_RS);
    assert_eq!(read(dir.path(), "src/utils.rs"), UTILS_RS);
}

#[tokio::test]
async fn a_collision_in_the_same_scope_is_refused() {
    let (dir, cg) = rust_project().await;
    let result = call(
        &cg,
        "tokensave_rename",
        json!({"symbol": "compute_total", "new_name": "other_fn", "dry_run": false}),
    )
    .await;
    assert_eq!(result["applied"], false, "{result:#}");
    let blockers = result["blockers"].as_array().unwrap();
    assert!(
        blockers
            .iter()
            .any(|b| b.as_str().unwrap().contains("already names")),
        "{blockers:?}"
    );
    assert_eq!(read(dir.path(), "src/utils.rs"), UTILS_RS);
}

#[tokio::test]
async fn an_invalid_identifier_is_refused() {
    let (_dir, cg) = rust_project().await;
    let result = call(
        &cg,
        "tokensave_rename",
        json!({"symbol": "compute_total", "new_name": "sum-total", "dry_run": false}),
    )
    .await;
    assert_eq!(result["applied"], false);
    assert!(result["refused"]
        .as_str()
        .unwrap()
        .contains("not an identifier"));
}

#[tokio::test]
async fn rename_preview_is_a_hidden_dry_run_alias() {
    let (dir, cg) = rust_project().await;
    let id = node_id(&cg, "compute_total").await;

    // Callable, and it never edits even when asked to.
    let plan = call(
        &cg,
        "tokensave_rename_preview",
        json!({"node_id": id, "new_name": "sum_total", "dry_run": false}),
    )
    .await;
    assert_eq!(plan["dry_run"], true, "{plan:#}");
    assert_eq!(plan["counts"]["exact"], 4, "{plan:#}");
    assert!(plan.get("applied").is_none());
    assert_eq!(read(dir.path(), "src/main.rs"), MAIN_RS);

    // Known to dispatch and permissions, absent from every tools/list.
    assert!(get_tool_definitions()
        .iter()
        .any(|d| d.name == "tokensave_rename_preview"));
    for toolset in [Toolset::Full, Toolset::Core] {
        let all: BTreeSet<String> = ["all".to_string()].into();
        let listed = get_listed_tool_definitions(toolset, &all);
        assert!(listed.iter().all(|d| d.name != "tokensave_rename_preview"));
    }
    let listed = get_listed_tool_definitions(Toolset::Full, &BTreeSet::new());
    assert!(listed.iter().any(|d| d.name == "tokensave_rename"));
    assert_eq!(tool_area("tokensave_rename"), "edit");
}

#[tokio::test]
async fn rename_description_states_its_limits() {
    let def = get_tool_definitions()
        .into_iter()
        .find(|d| d.name == "tokensave_rename")
        .unwrap();
    assert!(def.description.contains("NOT binding-aware"));
    for class in ["`exact`", "`heuristic`", "`ambiguous`", "`text_only`"] {
        assert!(def.description.contains(class), "{class}");
    }
    assert_eq!(def.annotations.unwrap()["readOnlyHint"], false);
}

#[tokio::test]
async fn unknown_node_reports_not_found() {
    let (_dir, cg) = rust_project().await;
    let result = handle_tool_call(
        &cg,
        "tokensave_rename",
        json!({"node_id": "nonexistent_id_12345", "new_name": "x"}),
        None,
        None,
    )
    .await
    .unwrap();
    let text = result.value["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Node not found"), "{text}");
}

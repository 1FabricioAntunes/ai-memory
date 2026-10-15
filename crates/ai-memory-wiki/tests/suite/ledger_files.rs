//! `Wiki::list_event_ledgers` and `Wiki::delete_event_ledger`: the only wiki
//! path that removes a hook event ledger.
//!
//! A ledger is a file, not a page, so the page-delete helpers do not fit. What
//! these tests pin is the gate that makes a file delete safe: a rotated name
//! AND ledger content, re-checked at delete time, inside one project's tree.

use ai_memory_core::{ProjectId, WorkspaceId};
use ai_memory_store::Store;
use ai_memory_wiki::{EventLedgerFile, Wiki};
use tempfile::TempDir;

const LEDGER: &str = "## [2025-01-03T10:00:00Z] user-prompt | hello\n";
const PAGE: &str = "# Notes\n\nA page that happens to be called log-2025-01.md.\n";

struct Fixture {
    _tmp: TempDir,
    wiki: Wiki,
    ws: WorkspaceId,
    proj: ProjectId,
    other: ProjectId,
}

async fn fixture() -> Fixture {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let ws = store
        .writer
        .get_or_create_workspace("default")
        .await
        .unwrap();
    let proj = store
        .writer
        .get_or_create_project(ws, "a", None)
        .await
        .unwrap();
    let other = store
        .writer
        .get_or_create_project(ws, "b", None)
        .await
        .unwrap();
    let wiki = Wiki::new(tmp.path(), store.writer.clone()).unwrap();
    for project in [proj, other] {
        std::fs::create_dir_all(wiki.project_root(ws, project)).unwrap();
    }
    Fixture {
        _tmp: tmp,
        wiki,
        ws,
        proj,
        other,
    }
}

fn put(fx: &Fixture, project: ProjectId, name: &str, body: &str) -> std::path::PathBuf {
    let path = fx.wiki.project_root(fx.ws, project).join(name);
    std::fs::write(&path, body).unwrap();
    path
}

#[tokio::test]
async fn listing_returns_only_rotated_files_that_open_like_a_ledger() {
    let fx = fixture().await;
    put(&fx, fx.proj, "log-2025-01.md", LEDGER);
    put(&fx, fx.proj, "log-2025-02.md", PAGE);
    put(&fx, fx.proj, "log.md", LEDGER);
    put(&fx, fx.proj, "log-2025-1.md", LEDGER);
    put(&fx, fx.proj, "notes.md", LEDGER);
    put(&fx, fx.other, "log-2025-03.md", LEDGER);

    let listed = fx.wiki.list_event_ledgers(fx.ws, fx.proj).unwrap();

    assert_eq!(
        listed,
        vec![EventLedgerFile {
            file_name: "log-2025-01.md".into(),
            bytes: LEDGER.len() as u64,
        }],
        "one project's listing is only its own ledger files"
    );
}

#[tokio::test]
async fn a_project_without_a_directory_lists_nothing() {
    let fx = fixture().await;
    std::fs::remove_dir_all(fx.wiki.project_root(fx.ws, fx.proj)).unwrap();
    assert!(
        fx.wiki
            .list_event_ledgers(fx.ws, fx.proj)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn deleting_a_ledger_removes_the_file_and_reports_its_size() {
    let fx = fixture().await;
    let path = put(&fx, fx.proj, "log-2025-01.md", LEDGER);

    let deleted = fx
        .wiki
        .delete_event_ledger(fx.ws, fx.proj, "log-2025-01.md")
        .await
        .unwrap();

    assert_eq!(deleted, Some(LEDGER.len() as u64));
    assert!(!path.exists());
    // Gone is not an error: a second call has nothing to delete.
    assert_eq!(
        fx.wiki
            .delete_event_ledger(fx.ws, fx.proj, "log-2025-01.md")
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn a_page_wearing_a_ledger_name_is_not_deleted_even_when_asked_by_name() {
    let fx = fixture().await;
    let path = put(&fx, fx.proj, "log-2025-01.md", PAGE);

    let deleted = fx
        .wiki
        .delete_event_ledger(fx.ws, fx.proj, "log-2025-01.md")
        .await
        .unwrap();

    assert_eq!(
        deleted, None,
        "the content gate is re-checked at delete time"
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), PAGE);
}

#[tokio::test]
async fn only_a_rotated_ledger_name_is_accepted() {
    let fx = fixture().await;
    let unrotated = put(&fx, fx.proj, "log.md", LEDGER);
    let page = put(&fx, fx.proj, "notes.md", LEDGER);
    let neighbour = put(&fx, fx.other, "log-2025-01.md", LEDGER);

    for name in [
        "log.md",
        "notes.md",
        "log-2025-01.md.bak",
        "../log-2025-01.md",
        "sub/log-2025-01.md",
        "..\\log-2025-01.md",
    ] {
        fx.wiki
            .delete_event_ledger(fx.ws, fx.proj, name)
            .await
            .expect_err(name);
    }

    assert!(unrotated.exists() && page.exists());
    assert!(neighbour.exists(), "no name reaches another project's file");
}

#[tokio::test]
async fn deleting_in_one_project_leaves_the_neighbours_ledger() {
    let fx = fixture().await;
    let mine = put(&fx, fx.proj, "log-2025-01.md", LEDGER);
    let theirs = put(&fx, fx.other, "log-2025-01.md", LEDGER);

    fx.wiki
        .delete_event_ledger(fx.ws, fx.proj, "log-2025-01.md")
        .await
        .unwrap();

    assert!(!mine.exists());
    assert!(theirs.exists());
}

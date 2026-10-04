//! Human-facing, read-only queries. This never constructs a board owner, reads
//! worker files, or shares a transaction with a worker operation.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::MetadataExt, path::Path, time::Duration};

pub(crate) const MAX_PAGE: usize = 32;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Page {
    pub offset: usize,
    pub limit: usize,
    pub through_sequence: Option<u64>,
    pub item_id: Option<u64>,
}

impl Default for Page {
    fn default() -> Self {
        Self {
            offset: 0,
            limit: 8,
            through_sequence: None,
            item_id: None,
        }
    }
}

/// Remove terminal escapes and directional controls before text crosses the
/// process boundary. Truncation is by Unicode scalar, never by a UTF-8 byte.
pub(crate) fn text(input: &str, max: usize) -> String {
    let mut out = String::new();
    let mut chars = input.chars().peekable();
    let mut count = 0;
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.next() {
                Some('[') => {
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') | Some('P') | Some('^') | Some('_') => {
                    while let Some(next) = chars.next() {
                        if next == '\u{7}'
                            || (next == '\u{1b}' && chars.next_if_eq(&'\\').is_some())
                        {
                            break;
                        }
                    }
                }
                _ => (),
            }
            continue;
        }
        if c.is_control() && c != '\n' && c != '\t'
            || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            continue;
        }
        if count == max {
            out.push('…');
            break;
        }
        out.push(c);
        count += 1;
    }
    out
}

fn clean(value: &Value, max: usize) -> Value {
    value
        .as_str()
        .map(|v| json!(text(v, max)))
        .unwrap_or(Value::Null)
}

fn body(raw: &str) -> Result<Value> {
    ensure!(
        raw.len() <= 1024 * 1024,
        "Saved board entry is too large for the view"
    );
    Ok(serde_json::from_str(raw)?)
}

pub(crate) fn private_file(path: &Path) -> Result<fs::Metadata> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file()
            && !meta.file_type().is_symlink()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.nlink() == 1
            && meta.mode() & 0o077 == 0,
        "Unsafe board view file"
    );
    Ok(meta)
}

fn collection(items: Vec<Value>, total: usize, page: Page, sequence: u64) -> Value {
    let end = page.offset.saturating_add(items.len());
    json!({"items":items,"total":total,"offset":page.offset,"limit":page.limit,
        "next_offset":(page.item_id.is_none() && end < total).then_some(end),"through_sequence":sequence,"item_id":page.item_id})
}

fn item_filter(page: Page) -> &'static str {
    // Keep exact lookup indexable; an optional `OR` predicate would make
    // SQLite scan the collection before locating a high record identity.
    if page.item_id.is_some() {
        "id=?2"
    } else {
        "?2 IS NULL"
    }
}

pub(crate) fn snapshot(run: &Path, selected: Option<&str>, requested: Page) -> Result<Value> {
    ensure!(
        (1..=MAX_PAGE).contains(&requested.limit) && requested.offset <= 1_000_000,
        "Invalid board view page"
    );
    ensure!(
        selected.is_none_or(|s| matches!(s, "tasks" | "shared" | "checks")),
        "Unknown view collection"
    );
    ensure!(
        requested.item_id.is_none_or(|id| id > 0
            && id <= i64::MAX as u64
            && selected.is_some()
            && requested.offset == 0
            && requested.through_sequence.is_none()),
        "Invalid exact board item lookup"
    );
    ensure!(
        fs::symlink_metadata(run)?.is_dir(),
        "Board view run cannot be a symbolic link"
    );
    let run = run.canonicalize()?;
    let dir = run.join("board");
    let meta = fs::symlink_metadata(&dir)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
        "Unsafe board view directory"
    );
    let path = dir.join("board.sqlite3");
    let before = private_file(&path)?;
    let mut companions = [false; 2];
    for (index, suffix) in ["-wal", "-shm"].iter().enumerate() {
        let auxiliary = dir.join(format!("board.sqlite3{suffix}"));
        match fs::symlink_metadata(&auxiliary) {
            Ok(_) => {
                private_file(&auxiliary)?;
                companions[index] = true;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    ensure!(
        !companions[0] || companions[1],
        "Board WAL is not ready for passive observation"
    );
    // SQLite's normal read-only WAL open may create missing companions. A
    // checkpointed database needs immutable mode to avoid that write. Never use
    // immutable mode with a live WAL: SQLite would ignore its committed pages.
    let immutable = !companions[0];
    let encoded = path
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"/._-".contains(byte) {
                (*byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect::<String>();
    let uri = format!(
        "file:{encoded}?mode=ro{}",
        if immutable { "&immutable=1" } else { "" }
    );
    let mut db = Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_URI,
    )?;
    let after = private_file(&path)?;
    ensure!(
        (before.dev(), before.ino()) == (after.dev(), after.ino()),
        "Board view file changed while opening"
    );
    db.busy_timeout(Duration::from_millis(20))?;
    db.execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF;")?;
    let tx = db.transaction()?;
    let sequence: u64 =
        tx.query_row("SELECT COALESCE(MAX(seq),0) FROM events", [], |r| r.get(0))?;
    let revision: u64 =
        tx.query_row("SELECT revision FROM context WHERE id=1", [], |r| r.get(0))?;
    let ceiling = requested.through_sequence.unwrap_or(sequence).min(sequence);
    let page = |name| {
        if selected == Some(name) {
            Page {
                limit: if requested.item_id.is_some() {
                    1
                } else {
                    requested.limit
                },
                ..requested
            }
        } else {
            Page::default()
        }
    };
    let tasks_page = page("tasks");
    let order = if selected == Some("tasks") {
        "id"
    } else {
        "CASE state WHEN 'claimed' THEN 0 WHEN 'available' THEN 1 ELSE 2 END,id"
    };
    let mut query = tx.prepare(&format!("SELECT id,owner,state,updated,substr(body,1,1048577) FROM tasks WHERE id<=?1 AND {} ORDER BY {order} LIMIT ?3 OFFSET ?4",item_filter(tasks_page)))?;
    let tasks = query.query_map(params![ceiling, tasks_page.item_id, tasks_page.limit, tasks_page.offset], |r|
        Ok((r.get::<_,u64>(0)?,r.get::<_,Option<usize>>(1)?,r.get::<_,String>(2)?,r.get::<_,u64>(3)?,r.get::<_,String>(4)?)))?
        .map(|row| -> Result<Value> {
            let (id,owner,state,version,raw) = row?;
            let b = body(&raw)?;
            let dependencies = b["dependencies"].as_array().map(|items| items.iter().filter_map(Value::as_u64).take(64).collect::<Vec<_>>()).unwrap_or_default();
            Ok(json!({"id":id,"title":clean(&b["title"],256),"description":clean(&b["description"],2048),
                "owner":owner,"state":text(&state,32),"version":version,"dependencies":dependencies,
                "kind":clean(&b["kind"],32),"interface":clean(&b["interface"],2048),
                "done_when":clean(&b["done_when"],1024),"handoff":clean(&b["handoff"],2048)}))
        }).collect::<Result<Vec<_>>>()?;
    drop(query);
    let task_total: usize =
        tx.query_row("SELECT COUNT(*) FROM tasks WHERE id<=?", [ceiling], |r| {
            r.get(0)
        })?;
    let mut query =
        tx.prepare("SELECT worker,state,summary,dependency,updated FROM workers ORDER BY worker")?;
    let workers = query.query_map([], |r| Ok(json!({"slot":r.get::<_,usize>(0)?,"reported_state":text(&r.get::<_,String>(1)?,32),
        "summary":text(&r.get::<_,String>(2)?,4096),"dependency":r.get::<_,Option<String>>(3)?.map(|s|text(&s,512)),"sequence":r.get::<_,u64>(4)?})))?
        .collect::<std::result::Result<Vec<_>,_>>()?;
    drop(query);
    // Agent ownership is independent of the visible task page.
    let mut query = tx.prepare(
        "SELECT owner,id FROM (
        SELECT owner,id,ROW_NUMBER() OVER(PARTITION BY owner ORDER BY id) AS owner_position
        FROM tasks WHERE state='claimed' AND owner IS NOT NULL
        ) WHERE owner_position<=64 ORDER BY owner,id LIMIT 2048",
    )?;
    let ownership = query
        .query_map([], |r| {
            Ok(json!({"slot":r.get::<_,usize>(0)?,"id":r.get::<_,u64>(1)?}))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(query);
    let mut query = tx.prepare("SELECT owner,COUNT(*) FROM tasks WHERE state='claimed' AND owner IS NOT NULL GROUP BY owner ORDER BY owner LIMIT 32")?;
    let ownership_counts = query
        .query_map([], |r| {
            Ok((r.get::<_, usize>(0)?.to_string(), r.get::<_, usize>(1)?))
        })?
        .collect::<std::result::Result<std::collections::BTreeMap<_, _>, _>>()?;
    drop(query);
    let shared_page = page("shared");
    let mut query = tx.prepare(&format!("SELECT id,worker,revision,kind,body FROM (
        SELECT id,worker,revision,'publication' AS kind,substr(body,1,1048577) AS body FROM publications
        UNION ALL SELECT seq,worker,revision,'finding',substr(body,1,1048577) FROM events WHERE kind='finding'
        ) WHERE id<=?1 AND {} ORDER BY id DESC LIMIT ?3 OFFSET ?4",item_filter(shared_page)))?;
    let shared_rows = query
        .query_map(
            params![
                ceiling,
                shared_page.item_id,
                shared_page.limit,
                shared_page.offset
            ],
            |r| {
                Ok((
                    r.get::<_, u64>(0)?,
                    r.get::<_, usize>(1)?,
                    r.get::<_, u64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(query);
    let publication_ids = shared_rows
        .iter()
        .filter(|row| row.3 == "publication")
        .map(|row| row.0.to_string())
        .collect::<Vec<_>>();
    let mut imports = std::collections::BTreeMap::<u64, Vec<usize>>::new();
    if !publication_ids.is_empty() {
        // One scan for the page, rather than scanning the event history for
        // every contribution. IDs are integers read from SQLite, never text.
        let mut query=tx.prepare(&format!("SELECT DISTINCT json_extract(body,'$.publication_id'),worker FROM events WHERE kind='apply' AND json_extract(body,'$.confirmed')=1 AND json_extract(body,'$.publication_id') IN ({}) ORDER BY worker",publication_ids.join(",")))?;
        for row in query.query_map([], |r| Ok((r.get::<_, u64>(0)?, r.get::<_, usize>(1)?)))? {
            let (id, worker) = row?;
            imports.entry(id).or_default().push(worker);
        }
    }
    let shared = shared_rows.into_iter().map(|(id,worker,revision,kind,raw)| -> Result<Value> {
        let b = body(&raw)?;
        let content = if kind == "finding" { &b["text"] } else { &b["summary"] };
        let file_count = b["files"].as_array().map_or(0,Vec::len);
        let files = b["files"].as_array().map(|files| files.iter().take(32).filter_map(|f|f["path"].as_str())
            .map(|s|text(s,512)).collect::<Vec<_>>()).unwrap_or_default();
        let imported_by = imports.remove(&id).unwrap_or_default();
        Ok(json!({"id":id,"kind":kind,"worker":worker,"revision":revision,"title":clean(content,160),
            "text":clean(content,4096),"files":files,"file_count":file_count,"imported_by":imported_by,
            "unfinished":clean(&b["unfinished"],2048)}))
    }).collect::<Result<Vec<_>>>()?;
    let shared_total: usize = tx.query_row("SELECT (SELECT COUNT(*) FROM publications WHERE id<=?1)+(SELECT COUNT(*) FROM events WHERE kind='finding' AND seq<=?1)",[ceiling],|r|r.get(0))?;
    let checks_page = page("checks");
    let mut query = tx.prepare(&format!("SELECT id,worker,revision,substr(body,1,1048577) FROM check_receipts WHERE id<=?1 AND {} ORDER BY id DESC LIMIT ?3 OFFSET ?4",item_filter(checks_page)))?;
    let checks = query.query_map(params![ceiling,checks_page.item_id,checks_page.limit,checks_page.offset],|r|Ok((r.get::<_,u64>(0)?,r.get::<_,usize>(1)?,r.get::<_,u64>(2)?,r.get::<_,String>(3)?)))?
        .map(|row| -> Result<Value> {
            let (id,worker,receipt_revision,raw)=row?;
            let b=body(&raw)?;
            Ok(json!({"id":id,"worker":worker,"revision":receipt_revision,"summary":clean(&b["summary"],2048),
                "passed":b["passed"].as_bool(),"inputs_unchanged":b["inputs_unchanged"].as_bool(),
                "reusable":Value::Null,
                "recorded_reusable":b["reusable"].as_bool(),"scope":clean(&b["scope"],128),
                "file_count":b["files"].as_object().map_or(0,|o|o.len()),"current_revision":receipt_revision==revision}))
        }).collect::<Result<Vec<_>>>()?;
    drop(query);
    let checks_total: usize = tx.query_row(
        "SELECT COUNT(*) FROM check_receipts WHERE id<=?",
        [ceiling],
        |r| r.get(0),
    )?;
    tx.commit().context("Finish read-only board snapshot")?;
    if immutable {
        let end = private_file(&path)?;
        ensure!(
            !dir.join("board.sqlite3-wal").try_exists()?
                && (
                    before.dev(),
                    before.ino(),
                    before.len(),
                    before.mtime(),
                    before.mtime_nsec()
                ) == (
                    end.dev(),
                    end.ino(),
                    end.len(),
                    end.mtime(),
                    end.mtime_nsec()
                ),
            "Board changed during its checkpointed read; retry observation"
        );
    }
    Ok(
        json!({"sequence":sequence,"revision":revision,"workers":workers,"ownership":ownership,"ownership_counts":ownership_counts,
        "tasks":collection(tasks,task_total,tasks_page,ceiling),"shared":collection(shared,shared_total,shared_page,ceiling),
        "checks":collection(checks,checks_total,checks_page,ceiling)}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Board;
    use fs2::FileExt;
    use std::{
        os::unix::fs::{PermissionsExt, symlink},
        sync::{Arc, Barrier},
    };

    fn fixture() -> (tempfile::TempDir, Board) {
        let root = tempfile::tempdir().unwrap();
        for name in ["baseline", "worker1", "worker2"] {
            fs::create_dir(root.path().join(name)).unwrap();
        }
        let board = Board::open(
            root.path(),
            &root.path().join("baseline"),
            [root.path().join("worker1"), root.path().join("worker2")],
        )
        .unwrap();
        (root, board)
    }

    fn task(board: &mut Board, n: usize) -> u64 {
        board.call(1,"delm_task_create",json!({"idempotency_key":format!("task-{n}"),"title":format!("Task {n}"),"description":"A focused requirement"}))
            .unwrap()["result"]["task_id"].as_u64().unwrap()
    }

    #[test]
    fn complete_pages_leave_worker_state_and_event_sequence_untouched() {
        let (root, mut board) = fixture();
        for n in 0..40 {
            task(&mut board, n);
        }
        let before = board.view().unwrap();
        let lock = fs::File::open(root.path().join("board/lock")).unwrap();
        lock.lock_exclusive().unwrap();
        // The observer reads concurrently even while the coordination owner is
        // locked: it never constructs Board or takes the owner lock.
        let first = snapshot(
            root.path(),
            Some("tasks"),
            Page {
                limit: 32,
                ..Page::default()
            },
        )
        .unwrap();
        let second = snapshot(
            root.path(),
            Some("tasks"),
            Page {
                offset: 32,
                limit: 32,
                ..Page::default()
            },
        )
        .unwrap();
        FileExt::unlock(&lock).unwrap();
        assert_eq!(first["tasks"]["total"], 40);
        assert_eq!(first["tasks"]["items"].as_array().unwrap().len(), 32);
        assert_eq!(second["tasks"]["items"].as_array().unwrap().len(), 8);
        assert_eq!(before, board.view().unwrap());
        assert_eq!(before["tasks"].as_array().unwrap().len(), 24);
        assert!(
            snapshot(
                root.path(),
                None,
                Page {
                    limit: 33,
                    ..Page::default()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn shared_entries_are_explicit_and_import_requires_confirmation() {
        let (root, mut board) = fixture();
        board
            .call(
                1,
                "delm_status",
                json!({"idempotency_key":"finding","summary":"Working","finding":"Contract ready"}),
            )
            .unwrap();
        let tx = board.db.transaction().unwrap();
        let id = super::super::event(
            &tx,
            1,
            "publication",
            &json!({"summary":"Published parser"}),
        )
        .unwrap();
        tx.execute(
            "INSERT INTO publications VALUES (?1,1,1,?2)",
            params![
                id,
                json!({"summary":"Published parser","files":[{"path":"parser.rs"}]}).to_string()
            ],
        )
        .unwrap();
        super::super::event(
            &tx,
            2,
            "apply",
            &json!({"publication_id":id,"confirmed":false}),
        )
        .unwrap();
        super::super::event(&tx, 1, "command", &json!({"text":"PRIVATE SHELL COMMAND"})).unwrap();
        tx.commit().unwrap();
        let before = snapshot(root.path(), None, Page::default()).unwrap();
        assert_eq!(before["shared"]["total"], 2);
        assert_eq!(before["shared"]["items"][0]["imported_by"], json!([]));
        assert!(!before.to_string().contains("PRIVATE SHELL COMMAND"));
        let tx = board.db.transaction().unwrap();
        super::super::event(
            &tx,
            2,
            "apply",
            &json!({"publication_id":id,"confirmed":true}),
        )
        .unwrap();
        tx.commit().unwrap();
        let after = snapshot(root.path(), None, Page::default()).unwrap();
        assert_eq!(after["shared"]["items"][0]["imported_by"], json!([2]));
    }

    #[test]
    fn page_membership_remains_stable_when_new_findings_arrive() {
        let (root, mut board) = fixture();
        for n in 0..12 {
            board
                .call(
                    1,
                    "delm_status",
                    json!({"idempotency_key":format!("f{n}"),"finding":format!("Finding {n}")}),
                )
                .unwrap();
        }
        let first = snapshot(root.path(), Some("shared"), Page::default()).unwrap();
        board
            .call(
                2,
                "delm_status",
                json!({"idempotency_key":"new","finding":"Newest"}),
            )
            .unwrap();
        let second = snapshot(
            root.path(),
            Some("shared"),
            Page {
                offset: 8,
                through_sequence: first["shared"]["through_sequence"].as_u64(),
                ..Page::default()
            },
        )
        .unwrap();
        assert_eq!(second["shared"]["total"], 12);
        assert_eq!(second["shared"]["items"].as_array().unwrap().len(), 4);
        let first_ids = first["shared"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].clone())
            .collect::<Vec<_>>();
        assert!(
            second["shared"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| !first_ids.contains(&v["id"]))
        );
    }

    #[test]
    fn readers_release_wal_transactions_before_returning() {
        let (root, mut board) = fixture();
        task(&mut board, 0);
        let barrier = Arc::new(Barrier::new(2));
        let reader_barrier = barrier.clone();
        let path = root.path().to_owned();
        let thread = std::thread::spawn(move || {
            reader_barrier.wait();
            for _ in 0..40 {
                snapshot(&path, None, Page::default()).unwrap();
            }
        });
        barrier.wait();
        for n in 1..40 {
            task(&mut board, n);
        }
        thread.join().unwrap();
        let checkpoint: (i64, i64, i64) = board
            .db
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!(checkpoint.0, 0);
        assert_eq!(
            snapshot(root.path(), None, Page::default()).unwrap()["tasks"]["total"],
            40
        );
    }

    #[test]
    fn unsafe_database_links_and_permissions_are_refused_without_repair() {
        let (root, board) = fixture();
        drop(board);
        let database = root.path().join("board/board.sqlite3");
        let real = root.path().join("original.db");
        fs::rename(&database, &real).unwrap();
        symlink(&real, &database).unwrap();
        assert!(snapshot(root.path(), None, Page::default()).is_err());
        fs::remove_file(&database).unwrap();
        fs::rename(&real, &database).unwrap();
        fs::set_permissions(&database, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(snapshot(root.path(), None, Page::default()).is_err());
        assert_eq!(fs::metadata(&database).unwrap().mode() & 0o777, 0o644);
    }

    #[test]
    fn terminal_text_is_safe_and_unicode_truncation_is_valid() {
        assert_eq!(
            text("\x1b[31mRed\x1b[0m \x1b]52;c;secret\x07界\u{202e}", 30),
            "Red 界"
        );
        assert_eq!(text("界🙂x", 2), "界🙂…");
    }

    #[test]
    fn overview_shows_current_work_even_after_completed_history_grows() {
        let (root, mut board) = fixture();
        for n in 0..12 {
            let id = task(&mut board, n);
            let claim = board
                .call(
                    1,
                    "delm_task_claim",
                    json!({"idempotency_key":format!("claim-{n}"),"task_id":id}),
                )
                .unwrap();
            board
                .call(
                    1,
                    "delm_task_finish",
                    json!({"idempotency_key":format!("done-{n}"),"task_id":id,
                "expected_version":claim["result"]["version"],"summary":"Finished requirement"}),
                )
                .unwrap();
        }
        let current = task(&mut board, 12);
        let overview = snapshot(root.path(), None, Page::default()).unwrap();
        assert_eq!(overview["tasks"]["items"][0]["id"], current);
        assert_eq!(overview["tasks"]["total"], 13);
        let history = snapshot(root.path(), Some("tasks"), Page::default()).unwrap();
        assert_eq!(history["tasks"]["items"][0]["state"], "done");
    }

    #[test]
    fn exact_lookup_finds_high_task_ids_and_older_shared_records_without_paging() {
        let (root, mut board) = fixture();
        let mut last = 0;
        for n in 0..40 {
            last = task(&mut board, n);
        }
        let selected = snapshot(
            root.path(),
            Some("tasks"),
            Page {
                item_id: Some(last),
                ..Page::default()
            },
        )
        .unwrap();
        assert_eq!(selected["tasks"]["items"].as_array().unwrap().len(), 1);
        assert_eq!(selected["tasks"]["items"][0]["id"], last);
        assert_eq!(selected["tasks"]["total"], 40);
        assert_eq!(selected["tasks"]["next_offset"], Value::Null);
        let first = board
            .db
            .query_row("SELECT MAX(seq) FROM events", [], |r| r.get::<_, u64>(0))
            .unwrap()
            + 2;
        for n in 0..32 {
            board
                .call(
                    1,
                    "delm_status",
                    json!({"idempotency_key":format!("old-{n}"),"finding":format!("Finding {n}")}),
                )
                .unwrap();
        }
        let selected = snapshot(
            root.path(),
            Some("shared"),
            Page {
                item_id: Some(first),
                ..Page::default()
            },
        )
        .unwrap();
        assert_eq!(selected["shared"]["items"][0]["text"], "Finding 0");
        assert_eq!(selected["shared"]["total"], 32);
        assert_eq!(selected["shared"]["next_offset"], Value::Null);
        assert!(
            snapshot(
                root.path(),
                None,
                Page {
                    item_id: Some(first),
                    ..Page::default()
                }
            )
            .is_err()
        );
    }
}

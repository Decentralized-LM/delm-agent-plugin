//! Bounded, model-facing discovery. Human view pagination remains independent.
//! Cursors fix the membership ceiling, not mutable task state. Filtered task
//! pages reject changed ownership so newly eligible work is never silently lost.
use super::{VIEW_LIMIT, compact, string};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u8,
    #[serde(default)]
    viewer: usize,
    collection: String,
    revision: u64,
    through: i64,
    after: i64,
    task_state: Option<String>,
    owner: Option<usize>,
    task_sequence: i64,
}

pub(super) fn counts(db: &Connection) -> Result<Value> {
    let mut result = serde_json::Map::new();
    for (name, query) in [
        ("tasks", "SELECT COUNT(*) FROM tasks"),
        ("publications", "SELECT COUNT(*) FROM publications"),
        (
            "findings",
            "SELECT COUNT(*) FROM events WHERE kind='finding'",
        ),
        ("checks", "SELECT COUNT(*) FROM check_receipts"),
    ] {
        let total: i64 = db.query_row(query, [], |row| row.get(0))?;
        result.insert(
            name.into(),
            json!({"total":total,"shown":total.min(VIEW_LIMIT),"has_more":total>VIEW_LIMIT}),
        );
    }
    Ok(Value::Object(result))
}

pub(super) fn list(db: &Connection, worker: usize, args: &Value) -> Result<Value> {
    let collection = string(args, "collection", 32)?;
    ensure!(
        matches!(collection, "tasks" | "publications" | "findings" | "checks"),
        "Unknown board collection"
    );
    let limit = match args.get("limit") {
        None => 12,
        Some(value) => value
            .as_i64()
            .filter(|n| (1..=VIEW_LIMIT).contains(n))
            .context("Discovery limit must be between 1 and 24")?,
    };
    let task_state = args
        .get("task_state")
        .map(|_| string(args, "task_state", 16))
        .transpose()?;
    ensure!(
        task_state.is_none_or(|s| matches!(s, "available" | "claimed" | "done")),
        "Unknown task state"
    );
    let owner = match args.get("owner") {
        None => None,
        Some(Value::String(value)) if value == "self" => Some(worker),
        Some(Value::String(value)) if value == "peer" => Some(0),
        _ => anyhow::bail!("Owner filter must be self or peer"),
    };
    ensure!(
        collection == "tasks" || (task_state.is_none() && owner.is_none()),
        "Task filters only apply to tasks"
    );
    let sequence: i64 = db.query_row("SELECT COALESCE(MAX(seq),0) FROM events", [], |row| {
        row.get(0)
    })?;
    let revision: u64 = db.query_row("SELECT revision FROM context WHERE id=1", [], |row| {
        row.get(0)
    })?;
    let prior = args.get("cursor").map(|_| -> Result<Cursor> {
        let cursor: Cursor = serde_json::from_str(string(args, "cursor", 1024)?)
            .context("Invalid discovery cursor; restart without a cursor")?;
        ensure!(cursor.version == 2 && cursor.viewer == worker && cursor.collection == collection
            && cursor.task_state.as_deref() == task_state && cursor.owner == owner,
            "Discovery cursor does not match this collection or filters; restart without a cursor");
        ensure!(cursor.revision == revision, "The request changed; restart discovery without a cursor");
        ensure!(cursor.after >= 0 && cursor.through >= cursor.after && cursor.through <= sequence,
            "Invalid discovery cursor boundary");
        Ok(cursor)
    }).transpose()?;
    let through = prior.as_ref().map_or(sequence, |cursor| cursor.through);
    let after = prior.as_ref().map_or(0, |cursor| cursor.after);
    let task_sequence: i64 = db.query_row(
        "SELECT COALESCE(MAX(updated),0) FROM tasks WHERE id<=?",
        [through],
        |row| row.get(0),
    )?;
    if collection == "tasks" && (task_state.is_some() || owner.is_some()) {
        ensure!(
            prior
                .as_ref()
                .is_none_or(|cursor| cursor.task_sequence == task_sequence),
            "Task state or ownership changed while paging; restart delm_list without a cursor for current work"
        );
    }
    let (mut items, total) = if collection == "tasks" {
        let filters = "id<=?1 AND (?3 IS NULL OR state=?3) AND (?4 IS NULL OR (?4=0 AND owner!=?5) OR owner=?4)";
        let total: i64 = db.query_row(
            &format!("SELECT COUNT(*) FROM tasks WHERE {filters} AND id>?2"),
            params![through, 0, task_state, owner, worker],
            |row| row.get(0),
        )?;
        let mut query = db.prepare(&format!("SELECT id,author,owner,state,body,updated,task_number FROM (SELECT tasks.*,ROW_NUMBER() OVER(ORDER BY id) AS task_number FROM tasks) WHERE {filters} AND id>?2 ORDER BY id LIMIT ?6"))?;
        let rows = query.query_map(
            params![through, after, task_state, owner, worker, limit + 1],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, usize>(1)?,
                    row.get::<_, Option<usize>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, u64>(6)?,
                ))
            },
        )?;
        let items = rows.map(|row| -> Result<Value> {
            let (id, author, owner, state, raw, version,task_number) = row?;
            let body: Value = serde_json::from_str(&raw)?;
            Ok(json!({"task_id":id,"task_number":task_number,"author":author,"owner":owner,"state":state,"version":version,
                "kind":body["kind"],"title":body["title"],"description":compact(&body["description"],256),
                "interface":compact(&body["interface"],256),"dependencies":body["dependencies"],
                "handoff":compact(&body["handoff"],256),"checkpoint":body["checkpoint"]}))
        }).collect::<Result<Vec<_>>>()?;
        (items, total)
    } else {
        let source = match collection {
            "publications" => "publications",
            "findings" => {
                "(SELECT seq AS id,worker,revision,body FROM events WHERE kind='finding')"
            }
            "checks" => "check_receipts",
            _ => unreachable!(),
        };
        let total: i64 = db.query_row(
            &format!("SELECT COUNT(*) FROM {source} WHERE id<=?"),
            [through],
            |row| row.get(0),
        )?;
        let mut query = db.prepare(&format!("SELECT id,worker,revision,body FROM {source} WHERE id<=?1 AND id>?2 ORDER BY id LIMIT ?3"))?;
        let rows = query.query_map(params![through, after, limit + 1], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, usize>(1)?,
                row.get::<_, u64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let items = rows.map(|row| -> Result<Value> {
            let (id,worker,revision,raw) = row?;
            let body: Value = serde_json::from_str(&raw)?;
            Ok(match collection {
                "publications" => json!({"publication_id":id,"worker":worker,"revision":revision,
                    "summary":compact(&body["summary"],512),"files":body["files"].as_array().map_or(0,Vec::len),
                    "dependencies":body["dependencies"],"unfinished":compact(&body["unfinished"],256)}),
                "findings" => json!({"finding_id":id,"sequence":id,"worker":worker,"revision":revision,"text":compact(&body["text"],1024)}),
                "checks" => json!({"receipt_id":id,"worker":worker,"request_revision":revision,
                    "summary":compact(&body["summary"],512),"passed":body["passed"],
                    "inputs_unchanged":body["inputs_unchanged"],"recorded_reusable":body["reusable"],
                    "reuse_requires_validation":true}),
                _ => unreachable!(),
            })
        }).collect::<Result<Vec<_>>>()?;
        (items, total)
    };
    let more = items.len() > limit as usize;
    items.truncate(limit as usize);
    let cursor = if more {
        let item = items.last().context("Discovery page was empty")?;
        let key = match collection {
            "tasks" => "task_id",
            "publications" => "publication_id",
            "findings" => "finding_id",
            _ => "receipt_id",
        };
        Some(serde_json::to_string(&Cursor {
            version: 2,
            viewer: worker,
            collection: collection.into(),
            revision,
            through,
            after: item[key]
                .as_i64()
                .context("Discovery record identity missing")?,
            task_state: task_state.map(str::to_owned),
            owner,
            task_sequence,
        })?)
    } else {
        None
    };
    Ok(
        json!({"collection":collection,"items":items,"total":total,"next_cursor":cursor,
        "through_sequence":through,"observed_sequence":sequence,"request_revision":revision,
        "new_events_available":sequence>through,"task_states_are_current":collection=="tasks"}),
    )
}

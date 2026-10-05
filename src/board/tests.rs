use super::*;
use std::collections::HashMap;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::{Arc, Barrier};

struct Fixture {
    _temp: tempfile::TempDir,
    run: PathBuf,
    baseline: PathBuf,
    workers: [PathBuf; 2],
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let run = temp.path().join("run");
        let baseline = temp.path().join("baseline");
        let workers = [temp.path().join("worker1"), temp.path().join("worker2")];
        for path in [&run, &baseline, &workers[0], &workers[1]] {
            fs::create_dir(path).unwrap();
        }
        Self {
            _temp: temp,
            run,
            baseline,
            workers,
        }
    }

    fn seed(&self, name: &str, bytes: &[u8]) {
        for root in [&self.baseline, &self.workers[0], &self.workers[1]] {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
    }

    fn board(&self) -> Board {
        let mut board = Board::open(&self.run, &self.baseline, self.workers.clone()).unwrap();
        for worker in 1..=2 {
            board.set_worker_policy(worker,&json!({"default_permissions":"test","permissions":{"test":{"filesystem":{self.workers[worker-1].to_string_lossy().as_ref():"write"}}}})).unwrap();
        }
        board
    }
}

fn publish(board: &mut Board, worker: usize, key: &str, paths: &[&str]) -> i64 {
    board
        .call(
            worker,
            "delm_publish",
            json!({"idempotency_key":key,"summary":"Useful partial contribution","paths":paths}),
        )
        .unwrap()["result"]["publication_id"]
        .as_i64()
        .unwrap()
}

#[test]
fn task_tools_explain_size_limits_and_reject_task_ids_as_publication_dependencies() {
    let definitions = tool_definitions();
    for name in ["delm_task_create", "delm_task_update", "delm_task_split"] {
        let tool = definitions
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap();
        let fields = if name == "delm_task_split" {
            &tool["inputSchema"]["properties"]["tasks"]["items"]["properties"]
        } else {
            &tool["inputSchema"]["properties"]
        };
        for (field, bound) in [
            ("title", 256),
            ("description", 2048),
            ("interface", 2048),
            ("earliest_contribution", 1024),
            ("done_when", 1024),
        ] {
            assert_eq!(fields[field]["maxLength"], bound);
            assert!(
                fields[field]["description"]
                    .as_str()
                    .unwrap()
                    .contains("UTF-8 bytes")
            );
        }
        assert!(
            fields["dependencies"]["description"]
                .as_str()
                .unwrap()
                .contains("not task IDs")
        );
    }
    let fixture = Fixture::new();
    let mut board = fixture.board();
    let task = create_task(&mut board, 1, "first-task", "implementation");
    let mut args = json!({"idempotency_key":"second-task","title":"Reusable parser", "description":"Contribute the parser interface", "interface":"é".repeat(1024), "dependencies":[task]});
    let error = board
        .call(1, "delm_task_create", args.clone())
        .unwrap_err()
        .to_string();
    assert!(error.contains("not task IDs"));
    assert!(error.contains("delm_list(collection=publications)"));
    assert_eq!(board.view().unwrap()["collections"]["tasks"]["total"], 1);
    let publication = publish(&mut board, 2, "parser-interface", &[]);
    args["dependencies"] = json!([publication]);
    args["interface"] = json!("é".repeat(1025));
    let error = board
        .call(1, "delm_task_create", args.clone())
        .unwrap_err()
        .to_string();
    assert!(error.contains("interface must be a string of at most 2048 bytes"));
    args["interface"] = json!("é".repeat(1024));
    board.call(1, "delm_task_create", args).unwrap();
    assert_eq!(board.view().unwrap()["collections"]["tasks"]["total"], 2);
}

#[test]
fn discovery_reaches_old_tasks_without_changing_claims_or_including_later_creations() {
    let fixture = Fixture::new();
    let mut board = fixture.board();
    let mut ids = Vec::new();
    for n in 0..40 {
        ids.push(create_task(
            &mut board,
            1,
            &format!("task-{n}"),
            "implementation",
        ));
        board
            .call(
                2,
                "delm_status",
                json!({"idempotency_key":format!("status-{n}"),"summary":"Inspecting"}),
            )
            .unwrap();
    }
    let before = board.view().unwrap();
    assert_eq!(before["tasks"].as_array().unwrap().len(), 24);
    assert_eq!(
        before["collections"]["tasks"],
        json!({"total":40,"shown":24,"has_more":true})
    );
    let mut page = board
        .call(2, "delm_list", json!({"collection":"tasks","limit":7}))
        .unwrap()["result"]
        .clone();
    assert_eq!(
        before,
        board.view().unwrap(),
        "Discovery must not append events or claim work"
    );
    assert_eq!(page["items"][0]["task_id"], ids[0]);
    assert_eq!(page["items"][1]["task_number"], 2);
    assert_ne!(
        page["items"][1]["task_id"], 2,
        "Intervening events make IDs sparse"
    );
    let later = create_task(&mut board, 2, "new-after-first-page", "implementation");
    let mut seen = Vec::new();
    loop {
        assert_eq!(page["total"], 40);
        seen.extend(
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["task_id"].as_i64().unwrap()),
        );
        if page["next_cursor"].is_null() {
            break;
        }
        page = board
            .call(
                2,
                "delm_list",
                json!({"collection":"tasks","limit":7,"cursor":page["next_cursor"]}),
            )
            .unwrap()["result"]
            .clone();
        assert_eq!(page["new_events_available"], true);
    }
    assert_eq!(seen, ids);
    assert!(!seen.contains(&later));
    let fresh = board
        .call(2, "delm_list", json!({"collection":"tasks"}))
        .unwrap();
    assert_eq!(fresh["result"]["total"], 41);
}

#[test]
fn filtered_discovery_refreshes_when_ownership_changes_and_cursors_cannot_cross_queries() {
    let fixture = Fixture::new();
    let mut board = fixture.board();
    let ids = (0..5)
        .map(|n| create_task(&mut board, 1, &format!("task-{n}"), "implementation"))
        .collect::<Vec<_>>();
    let page = board
        .call(
            1,
            "delm_list",
            json!({"collection":"tasks","task_state":"available","limit":2}),
        )
        .unwrap()["result"]
        .clone();
    let claim = claim_task(&mut board, 2, "claim-last", ids[4]);
    let error=board.call(1,"delm_list",json!({"collection":"tasks","task_state":"available","limit":2,"cursor":page["next_cursor"]})).unwrap_err();
    assert!(error.to_string().contains("ownership changed"));
    let claimed = board
        .call(
            1,
            "delm_list",
            json!({"collection":"tasks","task_state":"claimed","owner":"peer"}),
        )
        .unwrap();
    assert_eq!(claimed["result"]["total"], 1);
    assert_eq!(claimed["result"]["items"][0]["task_id"], ids[4]);
    let fresh = board
        .call(1, "delm_list", json!({"collection":"tasks","limit":2}))
        .unwrap()["result"]
        .clone();
    assert!(
        board
            .call(
                1,
                "delm_list",
                json!({"collection":"findings","cursor":fresh["next_cursor"]})
            )
            .is_err()
    );
    assert!(
        board
            .call(
                1,
                "delm_list",
                json!({"collection":"tasks","owner":"peer","cursor":fresh["next_cursor"]})
            )
            .is_err()
    );
    board.set_revision(2).unwrap();
    assert!(
        board
            .call(
                1,
                "delm_list",
                json!({"collection":"tasks","cursor":fresh["next_cursor"]})
            )
            .unwrap_err()
            .to_string()
            .contains("request changed")
    );
    assert_eq!(
        board.view().unwrap()["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["task_id"] == ids[4])
            .unwrap()["version"],
        claim
    );
}

#[test]
fn discovery_pages_all_explicit_sharing_and_receipts_with_bounded_summaries() {
    let fixture = Fixture::new();
    fixture.seed("main.js", b"ready");
    let mut board = fixture.board();
    for n in 0..27 {
        publish(&mut board, 1, &format!("publication-{n}"), &[]);
        board.call(2,"delm_status",json!({"idempotency_key":format!("finding-{n}"),"finding":format!("Useful finding {n}")})).unwrap();
        let begun=board.begin_check(1,json!({"idempotency_key":format!("begin-{n}"),"summary":format!("Focused check {n}"),"paths":["main.js"]}),||100).unwrap();
        board.finish_check(1,json!({"idempotency_key":format!("finish-{n}"),"snapshot_id":begun["result"]["snapshot_id"],"command_id":"test-command"}),&native_check(&fixture.workers[0],101,0)).unwrap();
    }
    for collection in ["publications", "findings", "checks"] {
        let before = board.view().unwrap();
        assert_eq!(before["collections"][collection]["total"], 27);
        let first = board
            .call(2, "delm_list", json!({"collection":collection,"limit":24}))
            .unwrap()["result"]
            .clone();
        let second = board
            .call(
                2,
                "delm_list",
                json!({"collection":collection,"limit":24,"cursor":first["next_cursor"]}),
            )
            .unwrap()["result"]
            .clone();
        assert_eq!(first["items"].as_array().unwrap().len(), 24);
        assert_eq!(second["items"].as_array().unwrap().len(), 3);
        assert!(second["next_cursor"].is_null());
        assert_eq!(before, board.view().unwrap());
        if collection == "checks" {
            assert_eq!(first["items"][0]["reuse_requires_validation"], true);
        }
    }
    for args in [
        json!({"collection":"tasks","limit":25}),
        json!({"collection":"tasks","limit":0}),
        json!({"collection":"findings","task_state":"available"}),
        json!({"collection":"tasks","cursor":"not-json"}),
    ] {
        assert!(board.call(1, "delm_list", args).is_err());
    }
}

#[test]
fn completion_artifact_selection_is_retained_and_cannot_name_external_paths() {
    let fixture = Fixture::new();
    let mut board = fixture.board();
    let declaration = json!({"idempotency_key":"result","expected_revision":1,"outcome":"complete","summary":"Rendered requested output","artifacts":["renders/demo.mp4","exports"]});
    let result = board.call(1, "delm_complete", declaration).unwrap();
    assert_eq!(
        result["result"]["artifacts"],
        json!(["renders/demo.mp4", "exports"])
    );
    let omitted = board.call(1, "delm_complete", json!({"idempotency_key":"unaccounted","expected_revision":1,"outcome":"complete","summary":"No artifact declaration"})).unwrap();
    assert!(omitted["result"].get("artifacts").is_none());
    let source_only = board.call(1, "delm_complete", json!({"idempotency_key":"source-only","expected_revision":1,"outcome":"complete","summary":"Only source changes requested","artifacts":[]})).unwrap();
    assert_eq!(source_only["result"]["artifacts"], json!([]));
    for path in ["../outside", "/tmp/output", "renders/../secret"] {
        assert!(board.call(1,"delm_complete",json!({"idempotency_key":format!("bad-{path}"),"expected_revision":1,"outcome":"complete","summary":"Invalid","artifacts":[path]})).is_err());
    }
}

#[test]
fn concurrent_claims_have_exactly_one_owner_and_replay_survives_reopen() {
    let fixture = Fixture::new();
    let mut board = fixture.board();
    let id = board.call(1,"delm_task_create",json!({"idempotency_key":"create","title":"Persistence","description":"Persist existing state"})).unwrap()["result"]["task_id"].as_i64().unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let handles = (1..=2)
        .map(|worker| {
            let mut peer = fixture.board();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let args = json!({"idempotency_key":"claim","task_id":id});
                (worker, peer.call(worker, "delm_task_claim", args))
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        results.iter().filter(|(_, result)| result.is_ok()).count(),
        1
    );
    let (owner, response) = results.iter().find(|(_, result)| result.is_ok()).unwrap();
    let mut reopened = fixture.board();
    let replay = reopened
        .call(
            *owner,
            "delm_task_claim",
            json!({"idempotency_key":"claim","task_id":id}),
        )
        .unwrap();
    assert_eq!(&replay, response.as_ref().unwrap());
    assert!(
        reopened
            .call(
                3 - *owner,
                "delm_task_finish",
                json!({"idempotency_key":"finish","task_id":id,"summary":"Done"})
            )
            .is_err()
    );
    assert!(
        reopened
            .call(
                *owner,
                "delm_task_claim",
                json!({"idempotency_key":"claim","task_id":id+1})
            )
            .is_err()
    );
}

#[test]
fn identity_is_native_and_components_do_not_complete_the_request() {
    let fixture = Fixture::new();
    let mut board = fixture.board();
    assert!(board.call(0, "delm_status", json!({})).is_err());
    assert!(
        board
            .call(1, "delm_status", json!({"agent_id":"2"}))
            .is_err()
    );
    let id = board
        .call(
            1,
            "delm_task_create",
            json!({"idempotency_key":"new","title":"UI","description":"Add filter"}),
        )
        .unwrap()["result"]["task_id"]
        .as_i64()
        .unwrap();
    let claim = board
        .call(
            1,
            "delm_task_claim",
            json!({"idempotency_key":"claim","task_id":id}),
        )
        .unwrap();
    let finished = board
        .call(
            1,
            "delm_task_finish",
            json!({"idempotency_key":"finish","task_id":id,"expected_version":claim["result"]["version"],"summary":"Filter available"}),
        )
        .unwrap();
    assert_eq!(finished["result"]["whole_task_complete"], false);
    board.set_revision(2).unwrap();
    assert_eq!(board.view().unwrap()["request_revision"], 2);
    assert!(board.set_revision(1).is_err());
}

#[test]
fn findings_are_append_only_and_waiting_requires_a_dependency() {
    let fixture = Fixture::new();
    let mut board = fixture.board();
    assert!(
        board
            .call(
                1,
                "delm_status",
                json!({"idempotency_key":"wait","state":"waiting"})
            )
            .is_err()
    );
    let first = board
        .call(
            1,
            "delm_status",
            json!({"idempotency_key":"one","finding":"Interface uses stable IDs"}),
        )
        .unwrap();
    board.call(2,"delm_status",json!({"idempotency_key":"two","state":"waiting","dependency":"persistence publication","finding":"Reload needs migration"})).unwrap();
    let replay = board
        .call(
            1,
            "delm_status",
            json!({"idempotency_key":"one","finding":"Interface uses stable IDs"}),
        )
        .unwrap();
    assert_eq!(first, replay);
    assert_eq!(
        board.view().unwrap()["findings"].as_array().unwrap().len(),
        2
    );
}

#[test]
fn completion_requires_current_observed_revision() {
    let fixture = Fixture::new();
    let mut board = fixture.board();
    board.set_revision(2).unwrap();
    let stale = json!({"idempotency_key":"finish","outcome":"complete","expected_revision":1,"summary":"Done","checks":[]});
    assert!(
        board
            .call(1, "delm_complete", stale)
            .unwrap_err()
            .to_string()
            .contains("stale")
    );
    let current = json!({"idempotency_key":"finish-current","outcome":"complete","expected_revision":2,"summary":"Done","checks":[]});
    assert_eq!(
        board.call(1, "delm_complete", current).unwrap()["result"]["expected_revision"],
        2
    );
}

#[test]
fn waiting_dependencies_bind_to_real_peer_work() {
    let fixture = Fixture::new();
    let mut board = fixture.board();
    let task = board
        .call(
            2,
            "delm_task_create",
            json!({"idempotency_key":"task","title":"Storage","description":"Save state"}),
        )
        .unwrap()["result"]["task_id"]
        .as_i64()
        .unwrap();
    board
        .call(
            2,
            "delm_task_claim",
            json!({"idempotency_key":"claim","task_id":task}),
        )
        .unwrap();
    let waiting = json!({"idempotency_key":"wait","expected_revision":1,"outcome":"waiting","summary":"Integrate peer storage","dependency":format!("task:{task}"),"checks":[]});
    board.call(1, "delm_complete", waiting).unwrap();
    assert_eq!(
        board.dependency_owner(&format!("task:{task}")).unwrap(),
        Some(2)
    );
    assert_eq!(board.dependency_owner("worker:2").unwrap(), Some(2));
    for (index, dependency) in ["worker:1", "worker:3", "task:99999", "storage publication"]
        .into_iter()
        .enumerate()
    {
        assert!(board.call(1,"delm_complete",json!({"idempotency_key":format!("invalid-{index}"),"expected_revision":1,"outcome":"waiting","summary":"Waiting","dependency":dependency})).is_err());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn immutable_publication_is_selective_and_preserves_binary_executable_and_deletion() {
    let fixture = Fixture::new();
    fixture.seed("main.txt", b"original");
    fixture.seed("remove.txt", b"remove");
    fs::write(fixture.workers[0].join("main.txt"), b"published").unwrap();
    fs::write(fixture.workers[0].join("asset.bin"), [0, 255, 128, 1]).unwrap();
    fs::set_permissions(
        fixture.workers[0].join("asset.bin"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fs::remove_file(fixture.workers[0].join("remove.txt")).unwrap();
    let mut board = fixture.board();
    let id = publish(
        &mut board,
        1,
        "publish",
        &["main.txt", "asset.bin", "remove.txt"],
    );
    fs::write(fixture.workers[0].join("main.txt"), b"author continues").unwrap();
    let expanded = board
        .call(
            2,
            "delm_expand",
            json!({"publication_id":id,"paths":["main.txt"]}),
        )
        .unwrap();
    assert_eq!(expanded["result"]["files"][0]["text"], "published");
    let first = board
        .call(
            2,
            "delm_apply",
            json!({"idempotency_key":"import-one","publication_id":id,"paths":["main.txt"]}),
        )
        .unwrap();
    assert_eq!(
        fs::read(fixture.workers[1].join("main.txt")).unwrap(),
        b"published"
    );
    assert_eq!(
        fs::read(fixture.workers[1].join("remove.txt")).unwrap(),
        b"remove"
    );
    assert!(!fixture.workers[1].join("asset.bin").exists());
    assert_eq!(
        board
            .call(
                2,
                "delm_apply",
                json!({"idempotency_key":"import-one","publication_id":id,"paths":["main.txt"]})
            )
            .unwrap(),
        first
    );
    board.call(2,"delm_apply",json!({"idempotency_key":"import-rest","publication_id":id,"paths":["asset.bin","remove.txt"]})).unwrap();
    assert_eq!(
        fs::read(fixture.workers[1].join("asset.bin")).unwrap(),
        [0, 255, 128, 1]
    );
    assert_eq!(
        fs::metadata(fixture.workers[1].join("asset.bin"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0o111
    );
    assert!(!fixture.workers[1].join("remove.txt").exists());
    assert_eq!(
        fs::read(fixture.baseline.join("main.txt")).unwrap(),
        b"original"
    );
    assert_eq!(
        fs::read(fixture.baseline.join("remove.txt")).unwrap(),
        b"remove"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn published_local_versions_are_compatible_but_unpublished_edits_are_refused() {
    let fixture = Fixture::new();
    fixture.seed("main.txt", b"baseline");
    let mut board = fixture.board();
    fs::write(fixture.workers[0].join("main.txt"), b"first").unwrap();
    let first = publish(&mut board, 1, "first", &["main.txt"]);
    board
        .call(
            2,
            "delm_apply",
            json!({"idempotency_key":"first-import","publication_id":first}),
        )
        .unwrap();
    fs::write(fixture.workers[0].join("main.txt"), b"second").unwrap();
    let second = publish(&mut board, 1, "second", &["main.txt"]);
    board
        .call(
            2,
            "delm_apply",
            json!({"idempotency_key":"second-import","publication_id":second}),
        )
        .unwrap();
    fs::write(
        fixture.workers[1].join("main.txt"),
        b"private unpublished edit",
    )
    .unwrap();
    let conflict = board
        .call(
            2,
            "delm_apply",
            json!({"idempotency_key":"conflict","publication_id":first}),
        )
        .unwrap_err();
    assert!(conflict.to_string().contains("diverged"));
    assert_eq!(
        fs::read(fixture.workers[1].join("main.txt")).unwrap(),
        b"private unpublished edit"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn tampered_publications_fail_before_import() {
    let fixture = Fixture::new();
    fixture.seed("main.txt", b"baseline");
    let mut board = fixture.board();
    fs::write(fixture.workers[0].join("main.txt"), b"published").unwrap();
    let id = publish(&mut board, 1, "publish", &["main.txt"]);
    let publication = board.publication(id).unwrap();
    let object = board
        .objects
        .path
        .join(publication["files"][0]["object"].as_str().unwrap());
    fs::set_permissions(&object, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(object, b"tampered!").unwrap();
    assert!(
        board
            .call(
                2,
                "delm_apply",
                json!({"idempotency_key":"import","publication_id":id})
            )
            .is_err()
    );
    assert_eq!(
        fs::read(fixture.workers[1].join("main.txt")).unwrap(),
        b"baseline"
    );
}

#[test]
fn traversal_symlink_and_git_paths_are_rejected() {
    let fixture = Fixture::new();
    fixture.seed("main.txt", b"baseline");
    let outside = fixture._temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret"), b"outside").unwrap();
    symlink(&outside, fixture.workers[0].join("escape")).unwrap();
    symlink(
        fixture.baseline.join("main.txt"),
        fixture.workers[0].join("linked"),
    )
    .unwrap();
    let mut board = fixture.board();
    for (index, path) in [
        "../outside/secret",
        "/etc/passwd",
        "escape/secret",
        "linked",
        ".git/config",
        ".GiT/config",
        "sub/.GIT/config",
        "a/../main.txt",
    ]
    .iter()
    .enumerate()
    {
        assert!(board.call(1,"delm_publish",json!({"idempotency_key":format!("unsafe-{index}"),"summary":"attempt","paths":[path]})).is_err(),"accepted {path}");
    }
    assert_eq!(fs::read(outside.join("secret")).unwrap(), b"outside");
}

#[cfg(target_os = "macos")]
#[test]
fn symlink_destination_and_changed_root_are_rejected() {
    let fixture = Fixture::new();
    fixture.seed("sub/file", b"baseline");
    let mut board = fixture.board();
    fs::write(fixture.workers[0].join("sub/file"), b"published").unwrap();
    let id = publish(&mut board, 1, "publish", &["sub/file"]);
    fs::rename(
        fixture.workers[1].join("sub"),
        fixture.workers[1].join("saved"),
    )
    .unwrap();
    symlink(fixture.baseline.join("sub"), fixture.workers[1].join("sub")).unwrap();
    assert!(
        board
            .call(
                2,
                "delm_apply",
                json!({"idempotency_key":"import","publication_id":id})
            )
            .is_err()
    );
    assert_eq!(
        fs::read(fixture.baseline.join("sub/file")).unwrap(),
        b"baseline"
    );
    fs::rename(
        &fixture.workers[1],
        fixture._temp.path().join("moved-worker"),
    )
    .unwrap();
    fs::create_dir(&fixture.workers[1]).unwrap();
    assert!(board.call(2, "delm_status", json!({})).is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn interrupted_import_is_never_reported_as_success_without_matching_files() {
    let fixture = Fixture::new();
    fixture.seed("main.txt", b"baseline");
    let mut board = fixture.board();
    fs::write(fixture.workers[0].join("main.txt"), b"published").unwrap();
    let id = publish(&mut board, 1, "publish", &["main.txt"]);
    let publication = board.publication(id).unwrap();
    let args = json!({"idempotency_key":"lost","publication_id":id});
    let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&args).unwrap()));
    let detail = json!({"publication_id":id,"files":publication["files"]});
    board.db.execute("INSERT INTO requests(worker,key,name,digest,state,detail) VALUES (2,'lost','delm_apply',?,'applying',?)",params![digest,serde_json::to_string(&detail).unwrap()]).unwrap();
    assert!(
        board
            .call(2, "delm_apply", args.clone())
            .unwrap_err()
            .to_string()
            .contains("unknown")
    );
    assert_eq!(
        fs::read(fixture.workers[1].join("main.txt")).unwrap(),
        b"baseline"
    );
    fs::write(fixture.workers[1].join("main.txt"), b"published").unwrap();
    assert_eq!(
        board.call(2, "delm_apply", args).unwrap()["result"]["confirmed"],
        true
    );
}

#[cfg(target_os = "macos")]
#[test]
fn board_transfers_cannot_bypass_native_read_denials_or_read_only_paths() {
    let fixture = Fixture::new();
    fixture.seed("private.txt", b"protected baseline");
    let mut board = fixture.board();
    fs::write(fixture.workers[0].join("private.txt"), b"contribution").unwrap();
    let id = publish(&mut board, 1, "before-restriction", &["private.txt"]);
    for worker in 1..=2 {
        board
            .set_worker_policy(
                worker,
                &json!({"default_permissions":"test","permissions":{"test":{"filesystem":{
                    fixture.workers[worker-1].to_string_lossy().as_ref():"write",
                    fixture.workers[worker-1].join("private.txt").to_string_lossy().as_ref():"deny"
                }}}}),
            )
            .unwrap();
    }
    assert!(board.call(1,"delm_publish",json!({"idempotency_key":"denied-publish","summary":"blocked","paths":["private.txt"]})).unwrap_err().to_string().contains("forbids"));
    assert!(
        board
            .call(2, "delm_expand", json!({"publication_id":id}))
            .unwrap_err()
            .to_string()
            .contains("forbids")
    );
    assert!(
        board
            .call(
                2,
                "delm_apply",
                json!({"idempotency_key":"denied-import","publication_id":id})
            )
            .unwrap_err()
            .to_string()
            .contains("forbids")
    );
    board
        .set_worker_policy(
            2,
            &json!({"default_permissions":"test","permissions":{"test":{"filesystem":{
                fixture.workers[1].to_string_lossy().as_ref():"write",
                fixture.workers[1].join("private.txt").to_string_lossy().as_ref():"read"
            }}}}),
        )
        .unwrap();
    assert!(
        board
            .call(2, "delm_expand", json!({"publication_id":id}))
            .is_ok()
    );
    assert!(
        board
            .call(
                2,
                "delm_apply",
                json!({"idempotency_key":"readonly-import","publication_id":id})
            )
            .is_err()
    );
    assert_eq!(
        fs::read(fixture.workers[1].join("private.txt")).unwrap(),
        b"protected baseline"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn case_and_unicode_aliases_cannot_bypass_restrictive_transfer_paths() {
    let fixture = Fixture::new();
    fixture.seed("private/secret.txt", b"protected");
    fixture.seed("caf\u{e9}/secret.txt", b"unicode protected");
    fs::create_dir(fixture.workers[0].join("readonly")).unwrap();
    fs::create_dir(fixture.workers[1].join("readonly")).unwrap();
    fs::create_dir(fixture.baseline.join("readonly")).unwrap();
    fs::write(fixture.workers[0].join("readonly/new.txt"), b"contribution").unwrap();
    let mut board = fixture.board();
    let denied_id = publish(&mut board, 1, "prior-private", &["PRIVATE/SECRET.TXT"]);
    let readonly_id = publish(&mut board, 1, "prior-readonly", &["READONLY/new.txt"]);
    for worker in 1..=2 {
        board
            .set_worker_policy(
                worker,
                &json!({"default_permissions":"test","permissions":{"test":{"filesystem":{
                    fixture.workers[worker-1].to_string_lossy().as_ref():"write",
                    fixture.workers[worker-1].join("private").to_string_lossy().as_ref():"deny",
                    fixture.workers[worker-1].join("caf\u{e9}").to_string_lossy().as_ref():"deny",
                    fixture.workers[worker-1].join("readonly").to_string_lossy().as_ref():"read",
                    fixture.workers[worker-1].join("future.txt").to_string_lossy().as_ref():"deny"
                }}}}),
            )
            .unwrap();
    }
    for (index, path) in ["PRIVATE/SECRET.TXT", "cafe\u{301}/secret.txt", "FUTURE.TXT"]
        .iter()
        .enumerate()
    {
        assert!(board.call(1, "delm_publish", json!({"idempotency_key":format!("alias-{index}"),"summary":"forbidden alias","paths":[path]})).is_err(), "published forbidden alias {path}");
    }
    assert!(
        board
            .call(2, "delm_expand", json!({"publication_id":denied_id}))
            .is_err()
    );
    assert!(
        board
            .call(
                2,
                "delm_apply",
                json!({"idempotency_key":"alias-import","publication_id":readonly_id})
            )
            .is_err()
    );
    assert!(!fixture.workers[1].join("readonly/new.txt").exists());
}

#[test]
fn oversized_sparse_file_is_refused_before_reading_its_body() {
    let fixture = Fixture::new();
    File::create(fixture.workers[0].join("huge.bin"))
        .unwrap()
        .set_len(4 * 1024 * 1024 * 1024 + 1)
        .unwrap();
    let mut board = fixture.board();
    let error = board.call(1, "delm_publish", json!({"idempotency_key":"huge","summary":"oversized","paths":["huge.bin"],"large_artifact":true})).unwrap_err();
    assert!(error.to_string().contains("4 GiB"), "{error}");
    assert!(
        board.view().unwrap()["publications"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

fn create_task(board: &mut Board, worker: usize, key: &str, kind: &str) -> i64 {
    board.call(worker, "delm_task_create", json!({"idempotency_key":key,"title":key,"description":"A useful bounded contribution","kind":kind})).unwrap()["result"]["task_id"].as_i64().unwrap()
}

fn claim_task(board: &mut Board, worker: usize, key: &str, task: i64) -> i64 {
    board
        .call(
            worker,
            "delm_task_claim",
            json!({"idempotency_key":key,"task_id":task}),
        )
        .unwrap()["result"]["version"]
        .as_i64()
        .unwrap()
}

#[test]
fn released_claims_fence_old_owners_and_same_owner_reclaims() {
    let fixture = Fixture::new();
    let mut board = fixture.board();
    let task = create_task(&mut board, 1, "task", "implementation");
    let old = claim_task(&mut board, 1, "claim", task);
    let release = json!({"idempotency_key":"release","task_id":task,"expected_version":old,"summary":"No code yet; the interface is ready"});
    let released = board.call(1, "delm_task_release", release.clone()).unwrap();
    assert_eq!(
        board.call(1, "delm_task_release", release).unwrap(),
        released
    );
    assert!(board.call(1,"delm_task_finish",json!({"idempotency_key":"late","task_id":task,"expected_version":old,"summary":"Stale completion"})).is_err());
    let new = claim_task(&mut board, 1, "reclaim", task);
    assert_ne!(old, new);
    assert!(board.call(1,"delm_task_finish",json!({"idempotency_key":"late2","task_id":task,"expected_version":old,"summary":"Still stale"})).is_err());
    board.call(1,"delm_task_release",json!({"idempotency_key":"release2","task_id":task,"expected_version":new,"summary":"Ready for peer"})).unwrap();
    let peer = claim_task(&mut board, 2, "peer", task);
    assert!(board.call(1,"delm_task_update",json!({"idempotency_key":"intrude","task_id":task,"expected_version":peer,"description":"Other owner"})).is_err());
    board.call(2,"delm_task_finish",json!({"idempotency_key":"done","task_id":task,"expected_version":peer,"summary":"Complete"})).unwrap();
}

#[test]
fn updated_interfaces_have_a_new_version_and_reject_stale_finishes() {
    let fixture = Fixture::new();
    let mut board = fixture.board();
    let task = create_task(&mut board, 1, "task", "implementation");
    let before = claim_task(&mut board, 1, "claim", task);
    let update = board.call(1,"delm_task_update",json!({"idempotency_key":"update","task_id":task,"expected_version":before,"interface":"parse(csv) returns Result<Contact[], ImportError[]>"})).unwrap();
    assert!(board.call(1,"delm_task_finish",json!({"idempotency_key":"old-finish","task_id":task,"expected_version":before,"summary":"Old contract"})).is_err());
    let expanded = board
        .call(2, "delm_expand", json!({"task_id":task}))
        .unwrap();
    assert_eq!(
        expanded["result"]["interface"],
        "parse(csv) returns Result<Contact[], ImportError[]>"
    );
    board.call(1,"delm_task_finish",json!({"idempotency_key":"finish","task_id":task,"expected_version":update["result"]["version"],"summary":"Current contract"})).unwrap();
}

#[test]
fn split_is_atomic_and_keeps_only_the_smaller_parent_claim() {
    let fixture = Fixture::new();
    let mut board = fixture.board();
    let task = create_task(&mut board, 1, "task", "implementation");
    let version = claim_task(&mut board, 1, "claim", task);
    let args = json!({"idempotency_key":"split","task_id":task,"expected_version":version,
        "remaining":{"title":"Parse input","description":"Keep the parser only"},
        "tasks":[{"title":"Save records","description":"Persist validated records","interface":"Accept parser output"},
        {"title":"Present errors","description":"Show field-level validation feedback"}]});
    let split = board.call(1, "delm_task_split", args.clone()).unwrap();
    assert_eq!(board.call(1, "delm_task_split", args).unwrap(), split);
    let tasks = board.view().unwrap()["tasks"].as_array().unwrap().clone();
    assert_eq!(tasks.len(), 3);
    assert_eq!(
        tasks.iter().filter(|t| t["state"] == "available").count(),
        2
    );
    assert_eq!(
        tasks.iter().find(|t| t["task_id"] == task).unwrap()["title"],
        "Parse input"
    );
    let child = split["result"]["created"][0]["task_id"].as_i64().unwrap();
    claim_task(&mut board, 2, "peer", child);
    let current = split["result"]["version"].as_i64().unwrap();
    let invalid = json!({"idempotency_key":"bad-split","task_id":task,"expected_version":current,
        "remaining":{"title":"Parse input","description":"Still own parser"},
        "tasks":[{"title":"Should roll back","description":"Valid first child"},{"title":"Invalid child"}]});
    assert!(board.call(1, "delm_task_split", invalid).is_err());
    assert_eq!(board.view().unwrap()["tasks"].as_array().unwrap().len(), 3);
    assert_eq!(
        board
            .call(1, "delm_expand", json!({"task_id":task}))
            .unwrap()["result"]["version"],
        current
    );
}

#[test]
fn only_one_temporary_integration_claim_exists() {
    let fixture = Fixture::new();
    let mut board = fixture.board();
    let first = create_task(&mut board, 1, "assembly1", "integration");
    let second = create_task(&mut board, 2, "assembly2", "integration");
    let version = claim_task(&mut board, 1, "claim1", first);
    assert!(
        board
            .call(
                2,
                "delm_task_claim",
                json!({"idempotency_key":"claim2","task_id":second})
            )
            .is_err()
    );
    board.call(1,"delm_task_release",json!({"idempotency_key":"handoff","task_id":first,"expected_version":version,"summary":"Peer can assemble from published pieces"})).unwrap();
    claim_task(&mut board, 2, "claim3", second);
}

#[test]
fn stopped_owner_releases_integration_without_losing_version_fence() {
    let fixture = Fixture::new();
    let mut board = fixture.board();
    let integration = create_task(&mut board, 1, "assembly", "integration");
    let prior = claim_task(&mut board, 1, "claim", integration);
    let own = create_task(&mut board, 2, "independent", "implementation");
    claim_task(&mut board, 2, "other", own);
    assert_eq!(
        board
            .release_worker_claims(1, "native turn failed")
            .unwrap(),
        vec![integration]
    );
    assert!(
        board
            .release_worker_claims(1, "native turn failed")
            .unwrap()
            .is_empty()
    );
    let new = claim_task(&mut board, 2, "takeover", integration);
    assert_ne!(prior, new);
    assert!(board.call(1,"delm_task_finish",json!({"idempotency_key":"stale","task_id":integration,"expected_version":prior,"summary":"Late declaration"})).is_err());
    assert_eq!(
        board
            .call(2, "delm_expand", json!({"task_id":own}))
            .unwrap()["result"]["owner"],
        2
    );
}

fn native_check(root: &Path, started: u64, exit: i64) -> HashMap<String, Value> {
    HashMap::from([(
        "test-command".into(),
        json!({"type":"commandExecution","id":"test-command","command":"node --test tests/parser.test.js","cwd":root,"status":if exit == 0 {"completed"} else {"failed"},"exitCode":exit,"_delm_revision":1,"_delm_started_sequence":started}),
    )])
}

#[test]
fn explicit_native_scopes_preserve_read_write_and_denied_boundaries() {
    let fixture = Fixture::new();
    fixture.seed("main.js", b"old");
    fixture.seed("private.txt", b"private");
    fs::write(fixture.workers[0].join("main.js"), b"new").unwrap();
    let mut board = fixture.board();
    board
        .set_worker_scopes(
            1,
            vec![
                FilesystemScope {
                    path: fixture.workers[0].clone(),
                    access: FilesystemAccess::Read,
                },
                FilesystemScope {
                    path: fixture.workers[0].join("private.txt"),
                    access: FilesystemAccess::Deny,
                },
            ],
        )
        .unwrap();
    let publication = publish(&mut board, 1, "read-allowed", &["main.js"]);
    assert!(board.input_snapshot(1, &["private.txt".into()]).is_err());
    board
        .set_worker_scopes(
            2,
            vec![FilesystemScope {
                path: fixture.workers[1].clone(),
                access: FilesystemAccess::Read,
            }],
        )
        .unwrap();
    let apply = json!({"idempotency_key":"apply","publication_id":publication});
    assert!(board.call(2, "delm_apply", apply.clone()).is_err());
    board
        .set_worker_scopes(
            2,
            vec![FilesystemScope {
                path: fixture.workers[1].join("main.js"),
                access: FilesystemAccess::Write,
            }],
        )
        .unwrap();
    board.call(2, "delm_apply", apply).unwrap();
    assert_eq!(
        fs::read(fixture.workers[1].join("main.js")).unwrap(),
        b"new"
    );
    for path in [PathBuf::from("relative"), fixture.workers[1].join("*.js")] {
        assert!(
            board
                .set_worker_scopes(
                    2,
                    vec![FilesystemScope {
                        path,
                        access: FilesystemAccess::Write
                    }]
                )
                .is_err()
        );
    }
    assert!(board.input_snapshot(2, &["main.js".into()]).is_ok());
    board.set_worker_scopes(2, vec![]).unwrap();
    assert!(board.input_snapshot(2, &["main.js".into()]).is_err());
}

#[test]
fn native_tool_receipts_reuse_scoped_success_without_claiming_a_process_exit() {
    use crate::evidence::{CommandCompletion, CommandEvidence, NativeHost};
    let fixture = Fixture::new();
    fixture.seed("main.js", b"ready");
    let mut board = fixture.board();
    let begun = board.begin_check(1,json!({"idempotency_key":"begin-native","summary":"Focused native check","paths":["main.js"]}),||10).unwrap();
    let mut command = CommandEvidence {
        host: NativeHost::Claude,
        id: "bash-7".into(),
        command: "node --test".into(),
        cwd: fixture.workers[0].clone(),
        revision: 1,
        started_sequence: Some(11),
        completion: CommandCompletion::NativeTool {
            result_ref: "7".into(),
            is_error: false,
            interrupted: false,
            background_task_id: Some("pending".into()),
            timed_out: false,
        },
    };
    let finish = json!({"idempotency_key":"finish-native","snapshot_id":begun["result"]["snapshot_id"],"command_id":"bash-7"});
    assert!(
        board
            .finish_check_with_evidence(
                1,
                finish.clone(),
                &HashMap::from([("bash-7".into(), command.clone())])
            )
            .is_err()
    );
    if let CommandCompletion::NativeTool {
        background_task_id, ..
    } = &mut command.completion
    {
        *background_task_id = None;
    }
    let receipt = board
        .finish_check_with_evidence(
            1,
            finish.clone(),
            &HashMap::from([("bash-7".into(), command)]),
        )
        .unwrap();
    assert_eq!(receipt["result"]["passed"], true);
    assert_eq!(
        receipt["result"]["evidence"]["completion"]["kind"],
        "native_tool"
    );
    assert_eq!(
        receipt["result"]["evidence"]["completion"]["result_ref"],
        "7"
    );
    assert!(receipt["result"]["native"].get("exitCode").is_none());
    assert!(
        receipt["result"]["evidence"]["completion"]
            .get("exit_code")
            .is_none()
    );
    assert_eq!(
        board
            .finish_check_with_evidence(1, finish, &HashMap::new())
            .unwrap(),
        receipt
    );
    let declaration = json!({"shared_checks":[receipt["result"]["receipt_id"]]});
    assert_eq!(
        board.shared_checks(2, &declaration, 1).unwrap()[0]["reusable"],
        true
    );
    fs::write(fixture.workers[1].join("main.js"), b"changed").unwrap();
    assert!(board.shared_checks(2, &declaration, 1).is_err());
}

#[test]
fn shared_evidence_requires_precommand_snapshot_and_matching_imported_inputs() {
    let fixture = Fixture::new();
    fixture.seed("parser.js", b"before");
    fixture.seed("package.json", b"{}");
    fs::write(fixture.workers[0].join("parser.js"), b"validated").unwrap();
    let mut board = fixture.board();
    let begun = board.begin_check(1,json!({"idempotency_key":"begin","summary":"Parser validation","paths":["parser.js","package.json"]}),||100).unwrap();
    let snapshot = begun["result"]["snapshot_id"].as_i64().unwrap();
    let finish = json!({"idempotency_key":"finish-check","snapshot_id":snapshot,"command_id":"test-command"});
    assert!(
        board
            .finish_check(1, finish.clone(), &native_check(&fixture.workers[0], 99, 0))
            .is_err()
    );
    assert!(
        board
            .finish_check(
                2,
                finish.clone(),
                &native_check(&fixture.workers[1], 101, 0)
            )
            .is_err()
    );
    assert!(
        board
            .finish_check(1, finish.clone(), &native_check(&fixture.baseline, 101, 0))
            .is_err()
    );
    let receipt = board
        .finish_check(
            1,
            finish.clone(),
            &native_check(&fixture.workers[0], 101, 0),
        )
        .unwrap();
    assert_eq!(
        board.finish_check(1, finish, &HashMap::new()).unwrap(),
        receipt
    );
    let id = receipt["result"]["receipt_id"].as_i64().unwrap();
    let declaration = json!({"shared_checks":[id]});
    assert!(board.shared_checks(2, &declaration, 1).is_err());
    fs::write(fixture.workers[1].join("parser.js"), b"validated").unwrap();
    assert_eq!(
        board.shared_checks(2, &declaration, 1).unwrap()[0]["worker"],
        1
    );
    // An unrelated edit does not invalidate this explicitly scoped check.
    fs::write(fixture.workers[1].join("unrelated.css"), b"body{}").unwrap();
    assert!(board.shared_checks(2, &declaration, 1).is_ok());
    fs::write(
        fixture.workers[1].join("package.json"),
        b"{\"type\":\"module\"}",
    )
    .unwrap();
    assert!(board.shared_checks(2, &declaration, 1).is_err());
    assert!(board.shared_checks(1, &declaration, 2).is_err());
    assert_eq!(
        board
            .call(2, "delm_expand", json!({"check_id":id}))
            .unwrap()["result"]["native"]["id"],
        "test-command"
    );
}

#[test]
fn failed_checks_and_changed_inputs_are_visible_but_not_reusable() {
    let fixture = Fixture::new();
    fixture.seed("input.js", b"before");
    let mut board = fixture.board();
    for (case, exit, changed) in [("failed", 1, false), ("changed", 0, true)] {
        let begun = board.begin_check(1,json!({"idempotency_key":format!("begin-{case}"),"summary":case,"paths":["input.js"]}),||100).unwrap();
        if changed {
            fs::write(fixture.workers[0].join("input.js"), b"after").unwrap();
        }
        let receipt = board.finish_check(1,json!({"idempotency_key":format!("finish-{case}"),"snapshot_id":begun["result"]["snapshot_id"],"command_id":"test-command"}),&native_check(&fixture.workers[0],101,exit)).unwrap();
        assert_eq!(receipt["result"]["reusable"], false);
        assert!(
            board
                .shared_checks(
                    1,
                    &json!({"shared_checks":[receipt["result"]["receipt_id"]]}),
                    1
                )
                .is_err()
        );
    }
    assert_eq!(
        board.view().unwrap()["check_receipts"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn wait_cursor_tracks_relevant_readiness_without_changing_board_views() {
    let fixture = Fixture::new();
    fixture.seed("source.txt", b"source");
    let mut board = fixture.board();
    let task = board
        .call(
            2,
            "delm_task_create",
            json!({"idempotency_key":"task", "title":"Storage", "description":"Storage"}),
        )
        .unwrap()["result"]["task_id"]
        .as_i64()
        .unwrap();
    let claim = board
        .call(
            2,
            "delm_task_claim",
            json!({"idempotency_key":"claim", "task_id":task}),
        )
        .unwrap();
    let declaration = json!({"idempotency_key":"wait", "expected_revision":1,"outcome":"waiting", "summary":"Waiting for storage", "dependency":format!("task:{task}")});
    board.call(1, "delm_complete", declaration.clone()).unwrap();
    let before = board.view().unwrap();
    let cursor = board.wait_cursor(&declaration).unwrap().unwrap();
    assert_eq!(board.view().unwrap(), before);
    assert!(!board.wait_ready(&cursor).unwrap());
    publish(&mut board, 2, "unrelated-publication", &["source.txt"]);
    assert!(
        !board.wait_ready(&cursor).unwrap(),
        "a peer publication does not finish the named task"
    );
    board.call(2,"delm_task_finish",json!({"idempotency_key":"finish", "task_id":task,"expected_version":claim["result"]["version"], "summary":"Storage ready"})).unwrap();
    assert!(board.wait_ready(&cursor).unwrap());
}

#[test]
fn newly_available_work_wakes_but_already_claimed_work_does_not() {
    let fixture = Fixture::new();
    let mut board = fixture.board();
    let declaration = json!({"outcome":"waiting", "dependency":"worker:2"});
    let cursor = board.wait_cursor(&declaration).unwrap().unwrap();
    let created = board
        .call(
            2,
            "delm_task_create",
            json!({"idempotency_key":"new", "title":"API", "description":"API"}),
        )
        .unwrap();
    assert!(board.wait_ready(&cursor).unwrap());
    let task = created["result"]["task_id"].as_i64().unwrap();
    let claim = board
        .call(
            2,
            "delm_task_claim",
            json!({"idempotency_key":"claim", "task_id":task}),
        )
        .unwrap();
    assert!(!board.wait_ready(&cursor).unwrap());
    board.call(2,"delm_task_release",json!({"idempotency_key":"release", "task_id":task,"expected_version":claim["result"]["version"],"summary":"Available to peer"})).unwrap();
    assert!(board.wait_ready(&cursor).unwrap());
    let next_wait = board.wait_cursor(&declaration).unwrap().unwrap();
    assert!(
        !board.wait_ready(&next_wait).unwrap(),
        "unchanged work must not cause another wake"
    );
}

#[test]
fn integration_availability_waits_for_the_current_assembler_to_finish() {
    let fixture = Fixture::new();
    let mut board = fixture.board();
    let current = board.call(2,"delm_task_create",json!({"idempotency_key":"current", "title":"First integration", "description":"Assemble", "kind":"integration"})).unwrap()["result"]["task_id"].clone();
    let claim = board
        .call(
            2,
            "delm_task_claim",
            json!({"idempotency_key":"claim", "task_id":current}),
        )
        .unwrap();
    board.call(2,"delm_task_create",json!({"idempotency_key":"next", "title":"Next integration", "description":"Assemble next", "kind":"integration"})).unwrap();
    let cursor = board
        .wait_cursor(&json!({"outcome":"waiting", "dependency":"worker:2"}))
        .unwrap()
        .unwrap();
    assert!(!board.wait_ready(&cursor).unwrap());
    board.call(2,"delm_task_finish",json!({"idempotency_key":"finish", "task_id":current,"expected_version":claim["result"]["version"],"summary":"Assembly complete"})).unwrap();
    assert!(
        board.wait_ready(&cursor).unwrap(),
        "previously unavailable integration is now claimable"
    );
}

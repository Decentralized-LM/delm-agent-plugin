use super::*;
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
    board
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
            json!({"idempotency_key":"finish","task_id":id,"summary":"Filter available"}),
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

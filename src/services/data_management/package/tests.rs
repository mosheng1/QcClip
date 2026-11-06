use super::*;
use crate::services::{
    capture::{CaptureService, mock::MockClipboardSource},
    database::FavoriteRepository,
};

fn service() -> (tempfile::TempDir, DataManagementService) {
    let root = tempfile::tempdir().unwrap();
    let service = DataManagementService::legacy_test_service(root.path());
    (root, service)
}
fn add(service: &DataManagementService, text: &str, favorite: bool) -> i64 {
    let capture = CaptureService::with_inline_threshold(
        service.database.clone(),
        service.blob_store.clone(),
        8,
    );
    let history = capture
        .store_captured_clipboard(&MockClipboardSource::text(text).capture())
        .unwrap();
    let db = service.database.connect().unwrap();
    let item: i64 = db
        .query_row(
            "SELECT item_id FROM clipboard_history WHERE id=?1",
            [history],
            |r| r.get(0),
        )
        .unwrap();
    drop(db);
    if favorite {
        FavoriteRepository::new(service.database.clone())
            .add_to_favorites(item)
            .unwrap();
    }
    item
}
fn favorites() -> Scope {
    Scope {
        favorites: true,
        ..Scope::default()
    }
}
fn count(service: &DataManagementService, table: &str) -> i64 {
    service
        .database
        .connect_read()
        .unwrap()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn favorite_replace_keeps_local_history_and_creates_recoverable_backup() {
    let (_source_root, source) = service();
    let item = add(&source, "家里的收藏与备注", true);
    add(&source, "不应出现在包内的历史", false);
    let db = source.database.connect().unwrap();
    db.execute("INSERT INTO tags(uuid,name,normalized_name,sort_order,created_at,updated_at) VALUES('tag','工作','工作',1,0,0)", []).unwrap();
    db.execute("INSERT INTO clipboard_item_tags(uuid,item_id,tag_id,created_at,updated_at) VALUES('link',?1,last_insert_rowid(),0,0)", [item]).unwrap();
    db.execute("INSERT INTO clipboard_item_notes(uuid,item_id,note_text,created_at,updated_at) VALUES('note',?1,'收藏备注',0,0)", [item]).unwrap();
    drop(db);
    let output = tempfile::tempdir().unwrap();
    let path = output.path().join("favorites.qcclip-data");
    source
        .export_package(
            &path,
            &ExportOptions {
                scope: favorites(),
                ..Default::default()
            },
            None,
        )
        .unwrap();
    let (_target_root, target) = service();
    add(&target, "公司本机历史和收藏", true);
    fs::write(target.data_root.join("config.json"), b"{}").unwrap();
    let package = target.inspect_package(&path, None).unwrap();
    assert_eq!(package.manifest.favorite_count, 1);
    assert_eq!(package.manifest.history_count, 0);
    let (backup, guard) = target
        .import_package(&package, &favorites(), ImportMode::Replace)
        .unwrap();
    drop(guard);
    assert_eq!(count(&target, "clipboard_history"), 1);
    assert_eq!(count(&target, "favorites"), 1);
    assert_eq!(count(&target, "tags"), 1);
    assert_eq!(count(&target, "clipboard_item_notes"), 1);
    assert_eq!(
        fs::read(target.data_root.join("config.json")).unwrap(),
        b"{}"
    );
    let before = target.inspect_package(&backup, None).unwrap();
    let (_, guard) = target
        .import_package(&before, &favorites(), ImportMode::Replace)
        .unwrap();
    drop(guard);
    let db = target.database.connect_read().unwrap();
    let text: String = db
        .query_row(
            "SELECT i.preview_text FROM favorites f JOIN clipboard_items i ON i.id=f.item_id",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(text, "公司本机历史和收藏");
}

#[test]
fn merge_deduplicates_and_subset_does_not_import_other_collections() {
    let (_source_root, source) = service();
    let source_item = add(&source, "两边相同的收藏", true);
    source.database.connect().unwrap().execute("INSERT INTO clipboard_item_notes(uuid,item_id,note_text,created_at,updated_at) VALUES('remote-note',?1,'远程备注',0,0)", [source_item]).unwrap();
    add(&source, "远程历史", false);
    let output = tempfile::tempdir().unwrap();
    let path = output.path().join("all.qcclip-data");
    source
        .export_package(
            &path,
            &ExportOptions {
                scope: Scope {
                    favorites: true,
                    history: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
        )
        .unwrap();
    let (_target_root, target) = service();
    let local_item = add(&target, "两边相同的收藏", true);
    target.database.connect().unwrap().execute("INSERT INTO clipboard_item_notes(uuid,item_id,note_text,created_at,updated_at) VALUES('local-note',?1,'本机备注',0,0)", [local_item]).unwrap();
    add(&target, "本机历史", false);
    let package = target.inspect_package(&path, None).unwrap();
    let (_, guard) = target
        .import_package(&package, &favorites(), ImportMode::Merge)
        .unwrap();
    drop(guard);
    assert_eq!(count(&target, "clipboard_items"), 2);
    assert_eq!(count(&target, "favorites"), 1);
    assert_eq!(count(&target, "clipboard_history"), 2);
    let note: String = target
        .database
        .connect_read()
        .unwrap()
        .query_row(
            "SELECT note_text FROM clipboard_item_notes WHERE item_id=?1",
            [local_item],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(note, "本机备注");
}

#[test]
fn failed_merge_keeps_original_data_and_does_not_schedule_a_future_import() {
    let (_root, source) = service();
    add(&source, "将要导入的收藏", true);
    let output = tempfile::tempdir().unwrap();
    let path = output.path().join("favorites.qcclip-data");
    source
        .export_package(
            &path,
            &ExportOptions {
                scope: favorites(),
                ..Default::default()
            },
            None,
        )
        .unwrap();
    let (_target_root, target) = service();
    add(&target, "本机收藏", true);
    let resource: String = source
        .database
        .connect_read()
        .unwrap()
        .query_row("SELECT relative_path FROM binary_blobs LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    let conflict = target.data_root.join(resource);
    fs::create_dir_all(conflict.parent().unwrap()).unwrap();
    fs::write(&conflict, "同路径的不同本机资源".as_bytes()).unwrap();
    let package = target.inspect_package(&path, None).unwrap();
    assert!(
        target
            .import_package(&package, &favorites(), ImportMode::Replace)
            .is_err()
    );
    assert_eq!(count(&target, "clipboard_history"), 1);
    assert_eq!(count(&target, "favorites"), 1);
    assert_eq!(
        fs::read(conflict).unwrap(),
        "同路径的不同本机资源".as_bytes()
    );
    assert!(
        !target
            .data_root
            .join(super::super::IMPORT_DIRECTORY)
            .join(super::super::IMPORT_MARKER)
            .exists()
    );
}

#[cfg(windows)]
#[test]
fn temporarily_shared_file_is_retried_until_its_handle_is_released() {
    use std::os::windows::fs::OpenOptionsExt;

    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("locked.json");
    let target = directory.path().join("moved.json");
    fs::write(&source, b"{}").unwrap();
    let mut handle = Some(
        fs::OpenOptions::new()
            .read(true)
            .share_mode(3)
            .open(&source)
            .unwrap(),
    );
    let mut attempts = 0;
    super::super::retry_permission_denied(|| {
        attempts += 1;
        let result = fs::rename(&source, &target);
        if attempts == 1 {
            assert_eq!(result.as_ref().unwrap_err().raw_os_error(), Some(32));
        }
        // 首次失败后释放占用；第二次实际文件操作应成功。
        drop(handle.take());
        result
    })
    .unwrap();
    drop(handle);
    assert!(attempts > 1);
    assert_eq!(fs::read(target).unwrap(), b"{}");
}

#[cfg(windows)]
#[test]
fn readonly_source_can_be_staged_and_flushed_without_changing_the_source() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.json");
    let target = directory.path().join("target.json");
    fs::write(&source, b"{}").unwrap();
    let mut permissions = fs::metadata(&source).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&source, permissions.clone()).unwrap();
    let result = super::super::copy_import_entry(&source, &target);
    assert!(fs::metadata(&source).unwrap().permissions().readonly());
    permissions.set_readonly(false);
    fs::set_permissions(&source, permissions).unwrap();
    result.unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"{}");
    assert!(!fs::metadata(&target).unwrap().permissions().readonly());
}

#[cfg(windows)]
#[test]
fn failed_application_rolls_back_and_can_be_retried_without_restart() {
    use std::os::windows::fs::OpenOptionsExt;

    let (_source_root, source) = service();
    add(&source, "远程收藏", true);
    let output = tempfile::tempdir().unwrap();
    let path = output.path().join("favorites.qcclip-data");
    source
        .export_package(
            &path,
            &ExportOptions {
                scope: favorites(),
                ..Default::default()
            },
            None,
        )
        .unwrap();
    let (_target_root, target) = service();
    add(&target, "保留本机历史", true);
    let state_path = target.data_root.join("state.json");
    fs::write(&state_path, b"{}").unwrap();
    let handle = fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&state_path)
        .unwrap();
    let package = target.inspect_package(&path, None).unwrap();
    let error = target
        .import_package(&package, &favorites(), ImportMode::Replace)
        .err()
        .expect("文件被持续占用时应报告导入失败");
    assert!(error.to_string().contains("占用"), "{error}");
    assert_eq!(count(&target, "favorites"), 1);
    let local_text: String = target
        .database
        .connect_read()
        .unwrap()
        .query_row(
            "SELECT i.preview_text FROM favorites f JOIN clipboard_items i ON i.id=f.item_id",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(local_text, "保留本机历史");
    assert_eq!(fs::read(&state_path).unwrap(), b"{}");
    assert!(!super::super::apply_pending_import(&target.data_root).unwrap());
    assert!(
        fs::read_dir(target.data_root.join(".package-backups"))
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("failed-import-"))
    );

    drop(handle);
    target.stage_package_directory(package.root()).unwrap();
    add(&target, "失败之后新增的本机历史", false);
    let (_, guard) = target
        .import_package(&package, &favorites(), ImportMode::Replace)
        .unwrap();
    drop(guard);
    assert_eq!(count(&target, "clipboard_history"), 2);
    let imported_text: String = target
        .database
        .connect_read()
        .unwrap()
        .query_row(
            "SELECT i.preview_text FROM favorites f JOIN clipboard_items i ON i.id=f.item_id",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(imported_text, "远程收藏");
    assert!(!super::super::apply_pending_import(&target.data_root).unwrap());
}

#[test]
fn invalid_pending_import_is_preserved_without_blocking_or_removing_local_data() {
    let (_root, target) = service();
    add(&target, "本机数据不能丢失", true);
    for state in ["invalid-marker", "missing-directory", "missing-database"] {
        let pending = target.data_root.join(super::super::IMPORT_DIRECTORY);
        fs::create_dir_all(&pending).unwrap();
        let name = if state == "invalid-marker" {
            "invalid".to_string()
        } else {
            uuid::Uuid::new_v4().to_string()
        };
        if state == "missing-database" {
            fs::create_dir(pending.join(&name)).unwrap();
        }
        fs::write(pending.join(super::super::IMPORT_MARKER), &name).unwrap();
        assert!(super::super::apply_pending_import(&target.data_root).is_err());
        assert_eq!(count(&target, "favorites"), 1);
        assert!(!super::super::apply_pending_import(&target.data_root).unwrap());
    }
    let preserved = fs::read_dir(target.data_root.join(".package-backups"))
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("failed-import-")
        })
        .count();
    assert_eq!(preserved, 3);
}

#[test]
fn independent_password_rejects_wrong_password_tampering_and_truncation() {
    let (_root, source) = service();
    add(&source, "需要加密的收藏", true);
    let output = tempfile::tempdir().unwrap();
    let path = output.path().join("secret.qcclip-data");
    let options = ExportOptions {
        scope: favorites(),
        ..Default::default()
    };
    source
        .export_package(&path, &options, Some("独立的密码"))
        .unwrap();
    assert!(DataManagementService::package_is_encrypted(&path).unwrap());
    assert!(source.inspect_package(&path, Some("错误密码")).is_err());
    let package = source.inspect_package(&path, Some("独立的密码")).unwrap();
    assert_eq!(package.manifest.favorite_count, 1);
    let original = fs::read(&path).unwrap();
    let second = output.path().join("second.qcclip-data");
    source
        .export_package(&second, &options, Some("独立的密码"))
        .unwrap();
    assert_ne!(original, fs::read(second).unwrap());
    let mut damaged = original.clone();
    damaged[55] ^= 1;
    fs::write(&path, damaged).unwrap();
    assert!(source.inspect_package(&path, Some("独立的密码")).is_err());
    fs::write(&path, &original[..original.len() - 20]).unwrap();
    assert!(source.inspect_package(&path, Some("独立的密码")).is_err());
    assert_eq!(count(&source, "favorites"), 1);
    assert!(
        !source
            .data_root
            .join(super::super::IMPORT_DIRECTORY)
            .exists()
    );
}

#[test]
fn published_archive_remains_importable_and_missing_resources_fail_before_apply() {
    let (_root, source) = service();
    add(&source, "旧归档外部存储的内容", true);
    let output = tempfile::tempdir().unwrap();
    let path = output.path().join("legacy.qcclip-data");
    source.export_data(&path).unwrap();
    assert!(!DataManagementService::package_is_encrypted(&path).unwrap());
    assert_eq!(
        source
            .inspect_package(&path, None)
            .unwrap()
            .manifest
            .favorite_count,
        1
    );
    let snapshot = tempfile::tempdir().unwrap();
    let manifest = snapshot::export(
        &source,
        snapshot.path(),
        &ExportOptions {
            scope: favorites(),
            ..Default::default()
        },
    )
    .unwrap();
    let db = rusqlite::Connection::open(snapshot.path().join("qcclip.sqlite3")).unwrap();
    let resource: String = db
        .query_row("SELECT relative_path FROM binary_blobs LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    drop(db);
    fs::remove_file(snapshot.path().join(resource)).unwrap();
    archive::write(snapshot.path(), &manifest, &path, None).unwrap();
    assert!(source.inspect_package(&path, None).is_err());
    assert_eq!(count(&source, "favorites"), 1);
}

#[test]
fn history_date_and_favorite_tag_filters_are_applied_before_packaging() {
    let (_root, source) = service();
    let item = add(&source, "带标签的收藏", true);
    add(&source, "未选中的收藏", true);
    let db = source.database.connect().unwrap();
    db.execute(
        "UPDATE clipboard_history SET first_seen_at=0,last_seen_at=0",
        [],
    )
    .unwrap();
    db.execute("INSERT INTO tags(uuid,name,normalized_name,sort_order,created_at,updated_at) VALUES('tag','选中','选中',1,0,0)", []).unwrap();
    let tag = db.last_insert_rowid();
    db.execute("INSERT INTO clipboard_item_tags(uuid,item_id,tag_id,created_at,updated_at) VALUES('link',?1,?2,0,0)", rusqlite::params![item, tag]).unwrap();
    drop(db);
    let output = tempfile::tempdir().unwrap();
    let path = output.path().join("selected.qcclip-data");
    source
        .export_package(
            &path,
            &ExportOptions {
                scope: Scope {
                    history: true,
                    favorites: true,
                    ..Default::default()
                },
                history_days: Some(1),
                favorite_tags: vec![tag],
            },
            None,
        )
        .unwrap();
    let package = source.inspect_package(&path, None).unwrap();
    assert_eq!(
        (
            package.manifest.history_count,
            package.manifest.favorite_count,
            package.manifest.filtered
        ),
        (0, 1, true)
    );
}

#[test]
fn gallery_merge_preserves_conflicting_images_and_replace_keeps_history() {
    use crate::services::image_library::ImageLibraryService;
    let (_source_root, source) = service();
    let gallery = ImageLibraryService::new(source.data_root.join("image_library"));
    gallery.create_group("常用图片", None).unwrap();
    gallery.create_group("空分组", None).unwrap();
    fs::write(
        source.data_root.join("image_library/常用图片/同名.png"),
        b"source-image",
    )
    .unwrap();
    let output = tempfile::tempdir().unwrap();
    let path = output.path().join("gallery.qcclip-data");
    let scope = Scope {
        gallery: true,
        ..Default::default()
    };
    source
        .export_package(
            &path,
            &ExportOptions {
                scope: scope.clone(),
                ..Default::default()
            },
            None,
        )
        .unwrap();
    let (_target_root, target) = service();
    add(&target, "保留本机历史", false);
    let local_gallery = ImageLibraryService::new(target.data_root.join("image_library"));
    local_gallery.create_group("常用图片", None).unwrap();
    local_gallery.create_group("本机其他图片", None).unwrap();
    fs::write(
        target.data_root.join("image_library/常用图片/同名.png"),
        b"local-image",
    )
    .unwrap();
    let package = target.inspect_package(&path, None).unwrap();
    let (_, guard) = target
        .import_package(&package, &scope, ImportMode::Merge)
        .unwrap();
    drop(guard);
    assert_eq!(local_gallery.load_groups().unwrap().len(), 3);
    let files = fs::read_dir(target.data_root.join("image_library/常用图片"))
        .unwrap()
        .map(|f| fs::read(f.unwrap().path()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 2);
    assert!(files.contains(&b"local-image".to_vec()) && files.contains(&b"source-image".to_vec()));
    let (_, guard) = target
        .import_package(&package, &scope, ImportMode::Merge)
        .unwrap();
    drop(guard);
    assert_eq!(
        fs::read_dir(target.data_root.join("image_library/常用图片"))
            .unwrap()
            .count(),
        2
    );
    let (_, guard) = target
        .import_package(&package, &scope, ImportMode::Replace)
        .unwrap();
    drop(guard);
    assert_eq!(local_gallery.load_groups().unwrap().len(), 2);
    assert!(target.data_root.join("image_library/空分组").is_dir());
    assert_eq!(
        fs::read(target.data_root.join("image_library/常用图片/同名.png")).unwrap(),
        b"source-image"
    );
    assert_eq!(count(&target, "clipboard_history"), 1);
    assert_eq!(count(&target, "binary_blobs"), 1);
}

#[test]
fn archive_rejects_unsafe_paths_and_duplicated_paths() {
    use std::io::Write;
    let (_root, source) = service();
    let manifest = snapshot::manifest(&source.data_root, favorites(), false).unwrap();
    let json = serde_json::to_vec(&manifest).unwrap();
    let output = tempfile::tempdir().unwrap();
    let path = output.path().join("unsafe.qcclip-data");
    for names in [
        vec!["data/../../outside"],
        vec!["data/stream:secret"],
        vec!["data/a", "data/A"],
        vec!["data/a./file"],
        vec!["data//file"],
    ] {
        let mut plain = Vec::new();
        plain.write_all(&(json.len() as u32).to_le_bytes()).unwrap();
        plain.write_all(&json).unwrap();
        plain
            .write_all(&(names.len() as u32).to_le_bytes())
            .unwrap();
        for name in names {
            plain.write_all(&(name.len() as u32).to_le_bytes()).unwrap();
            plain.write_all(&0u64.to_le_bytes()).unwrap();
            use sha2::{Digest, Sha256};
            plain.write_all(&Sha256::digest(b"")).unwrap();
            plain.write_all(name.as_bytes()).unwrap();
        }
        let compressed = zstd::stream::encode_all(plain.as_slice(), 1).unwrap();
        let mut bytes = b"QCCLIPPKG2\n\0".to_vec();
        bytes.extend(compressed);
        fs::write(&path, bytes).unwrap();
        assert!(source.inspect_package(&path, None).is_err());
    }
    assert!(
        !source
            .data_root
            .join(super::super::IMPORT_DIRECTORY)
            .exists()
    );
}

#[test]
fn webdav_packages_use_manual_requests_and_do_not_require_pairing_or_sync_indexes() {
    use crate::services::sync::{config::DavConfig, webdav::Client};
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
        time::Duration,
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let bytes = b"local-and-cloud-use-the-same-file".to_vec();
    let expected = bytes.clone();
    let worker = thread::spawn(move || {
        let mut stored = None;
        let mut requests = Vec::new();
        for _ in 0..6 {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && std::time::Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("本机 WebDAV 测试未收到请求：{error}"),
                }
            };
            // Windows 上 accept 得到的连接会继承监听器的非阻塞模式。
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                header.push(byte[0]);
            }
            let header = String::from_utf8(header).unwrap();
            let mut request = header.lines().next().unwrap().split_whitespace();
            let method = request.next().unwrap().to_owned();
            let path = request.next().unwrap().to_owned();
            let length = header
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                        .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            requests.push((method.clone(), path.clone()));
            let (status, response) = match method.as_str() {
                "MKCOL" => (201, Vec::new()),
                "PUT" => {
                    assert_eq!(body, expected);
                    stored = Some(path.clone());
                    (201, Vec::new())
                }
                "PROPFIND" => {
                    assert!(header.to_lowercase().contains("depth: 1"));
                    let path = stored.as_ref().unwrap();
                    (207,format!("<d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>{path}</d:href><d:propstat><d:prop><d:getcontentlength>{}</d:getcontentlength><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>", expected.len() + 1).into_bytes())
                }
                "GET" => {
                    assert_eq!(Some(&path), stored.as_ref());
                    (200, expected.clone())
                }
                "DELETE" => {
                    assert_eq!(Some(&path), stored.as_ref());
                    (204, Vec::new())
                }
                _ => panic!("出现非预期请求：{method}"),
            };
            write!(
                stream,
                "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            )
            .unwrap();
            stream.write_all(&response).unwrap();
        }
        requests
    });
    let config = DavConfig {
        url: format!("http://{address}/"),
        ..Default::default()
    };
    let client = Client::new(&config).unwrap();
    let output = tempfile::tempdir().unwrap();
    let path = output.path().join("upload.qcclip-data");
    fs::write(&path, &bytes).unwrap();
    CloudPackage::upload(&client, &path).unwrap();
    let files = CloudPackage::list(&client).unwrap();
    assert_eq!(files.len(), 1);
    assert_ne!(files[0].bytes, bytes.len() as u64);
    let download = output.path().join("download.qcclip-data");
    files[0].download(&client, &download).unwrap();
    assert_eq!(fs::read(download).unwrap(), bytes);
    files[0].delete(&client).unwrap();
    let requests = worker.join().unwrap();
    assert_eq!(
        requests.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        ["MKCOL", "MKCOL", "PUT", "PROPFIND", "GET", "DELETE"]
    );
    assert!(
        requests
            .iter()
            .all(|(_, path)| !path.contains("indexes") && !path.contains("objects"))
    );
}

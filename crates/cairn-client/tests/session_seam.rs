// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

#![cfg(windows)]

use cairn_client::client::ClientError;
use cairn_client::codec::{encode_frame, try_decode_frame};
use cairn_client::message::{
    ClientRequest, Envelope, QueryDefaults, ResponsePayload, SortColumn, WIRE_GUARD,
};
use cairn_client::{CairnClient, ServiceSession};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::{oneshot, Mutex};

/// The fields a `Query` carries, captured to compare a request with its replay.
type SentQuery = (u64, String, QueryDefaults, SortColumn, bool, usize);

fn unique_pipe_name(tag: &str) -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(1);
    let seq = NEXT.fetch_add(1, Ordering::Relaxed);
    format!(
        r"\\.\pipe\cairn.client.seam.{tag}.{}.{seq}",
        std::process::id()
    )
}

async fn read_one_request(
    server: &mut NamedPipeServer,
    buf: &mut Vec<u8>,
) -> Option<ClientRequest> {
    let mut tmp = [0u8; 8192];
    loop {
        if let Some((consumed, req)) = try_decode_frame::<ClientRequest>(buf).expect("decode req") {
            buf.drain(0..consumed);
            return Some(req);
        }
        let n = server.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
}

async fn write_response(server: &mut NamedPipeServer, payload: ResponsePayload) {
    let frame = encode_frame(&Envelope {
        volumes: Vec::new(),
        payload,
    })
    .expect("encode response");
    server.write_all(&frame).await.expect("write response");
    server.flush().await.expect("flush response");
}

#[tokio::test]
async fn session_reuses_one_connection_per_slot_across_rpcs() {
    let pipe = unique_pipe_name("reuse");
    let server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&pipe)
        .expect("create server");

    let accepts = Arc::new(AtomicUsize::new(0));
    let hellos = Arc::new(AtomicUsize::new(0));
    let accepts_srv = accepts.clone();
    let hellos_srv = hellos.clone();
    let pipe_srv = pipe.clone();

    let server_task = tokio::spawn(async move {
        let mut server = server;
        let mut handlers = Vec::new();
        // The session keeps a control connection beside the query connection, so accept two
        // clients and serve them concurrently.
        for _ in 0..2 {
            server.connect().await.expect("accept conn");
            accepts_srv.fetch_add(1, Ordering::SeqCst);
            let next = ServerOptions::new()
                .first_pipe_instance(false)
                .create(&pipe_srv)
                .expect("spare pipe");
            let hellos = hellos_srv.clone();
            handlers.push(tokio::spawn(async move {
                let mut buf = Vec::new();
                while let Some(req) = read_one_request(&mut server, &mut buf).await {
                    let reply = match req {
                        ClientRequest::Hello { .. } => {
                            hellos.fetch_add(1, Ordering::SeqCst);
                            ResponsePayload::HelloAck {
                                server_version: WIRE_GUARD as u32,
                                caller_is_admin: false,
                            }
                        }
                        ClientRequest::GetStatus => ResponsePayload::StatusAck,
                        ClientRequest::Query { query_id, .. } => ResponsePayload::QueryResult {
                            query_id,
                            session_id: 77,
                            total_matches: 1,
                            total_bytes: 128,
                            first_page: Vec::new(),
                        },
                        ClientRequest::GetRows {
                            session_id, offset, ..
                        } => ResponsePayload::RowsResult {
                            session_id,
                            offset,
                            rows: Vec::new(),
                        },
                        ClientRequest::GetTree { path, .. } => ResponsePayload::TreeResult {
                            path,
                            entries: Vec::new(),
                        },
                        other => panic!("unexpected request: {other:?}"),
                    };
                    write_response(&mut server, reply).await;
                }
            }));
            server = next;
        }
        for handler in handlers {
            let _ = handler.await;
        }
    });

    let session = ServiceSession::for_pipe(pipe);
    session.get_status().await.expect("status");
    session
        .query(
            1,
            "notes",
            QueryDefaults::default(),
            SortColumn::Name,
            true,
            50,
        )
        .await
        .expect("query");
    session.get_rows(77, 50, 50).await.expect("get_rows");
    session.get_tree(r"C:\", 3).await.expect("get_tree");
    drop(session);

    server_task.await.expect("server finish");
    assert_eq!(
        accepts.load(Ordering::SeqCst),
        2,
        "one query slot and one control slot"
    );
    assert_eq!(
        hellos.load(Ordering::SeqCst),
        2,
        "each slot greets its connection once and reuses it"
    );
}

#[tokio::test]
async fn session_reconnects_once_after_cached_pipe_closes() {
    let pipe = unique_pipe_name("reconnect");
    let first_server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&pipe)
        .expect("first server");

    let accepts = Arc::new(AtomicUsize::new(0));
    let hellos = Arc::new(AtomicUsize::new(0));
    let accepts_srv = accepts.clone();
    let hellos_srv = hellos.clone();
    let pipe_srv = pipe.clone();

    let server_task = tokio::spawn(async move {
        let mut s1 = first_server;
        s1.connect().await.expect("accept 1");
        accepts_srv.fetch_add(1, Ordering::SeqCst);
        let mut s2 = ServerOptions::new()
            .first_pipe_instance(false)
            .create(&pipe_srv)
            .expect("second server");

        let mut buf = Vec::new();
        while let Some(req) = read_one_request(&mut s1, &mut buf).await {
            match req {
                ClientRequest::Hello { .. } => {
                    hellos_srv.fetch_add(1, Ordering::SeqCst);
                    write_response(
                        &mut s1,
                        ResponsePayload::HelloAck {
                            server_version: WIRE_GUARD as u32,
                            caller_is_admin: false,
                        },
                    )
                    .await;
                }
                ClientRequest::GetStatus => {
                    write_response(&mut s1, ResponsePayload::StatusAck).await;
                    break;
                }
                other => panic!("unexpected req on conn 1: {other:?}"),
            }
        }
        drop(s1);

        s2.connect().await.expect("accept 2");
        accepts_srv.fetch_add(1, Ordering::SeqCst);
        buf.clear();
        while let Some(req) = read_one_request(&mut s2, &mut buf).await {
            let reply = match req {
                ClientRequest::Hello { .. } => {
                    hellos_srv.fetch_add(1, Ordering::SeqCst);
                    ResponsePayload::HelloAck {
                        server_version: WIRE_GUARD as u32,
                        caller_is_admin: false,
                    }
                }
                ClientRequest::GetRows {
                    session_id, offset, ..
                } => ResponsePayload::RowsResult {
                    session_id,
                    offset,
                    rows: Vec::new(),
                },
                other => panic!("unexpected req on conn 2: {other:?}"),
            };
            write_response(&mut s2, reply).await;
        }
    });

    let session = ServiceSession::for_pipe(pipe);
    session.get_status().await.expect("initial status");
    let rows = session
        .get_rows(9, 100, 50)
        .await
        .expect("get_rows should transparently reconnect");
    assert!(matches!(
        rows.payload,
        ResponsePayload::RowsResult {
            session_id: 9,
            offset: 100,
            ..
        }
    ));
    drop(session);

    server_task.await.expect("server finish");
    assert_eq!(accepts.load(Ordering::SeqCst), 2);
    assert_eq!(hellos.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_retried_query_carries_the_same_request() {
    let pipe = unique_pipe_name("query_replay");
    let first_server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&pipe)
        .expect("first server");

    let queries: Arc<Mutex<Vec<SentQuery>>> = Arc::new(Mutex::new(Vec::new()));
    let queries_srv = queries.clone();
    let pipe_srv = pipe.clone();

    let server_task = tokio::spawn(async move {
        let mut s1 = first_server;
        s1.connect().await.expect("accept 1");
        let mut s2 = ServerOptions::new()
            .first_pipe_instance(false)
            .create(&pipe_srv)
            .expect("second server");

        let mut buf = Vec::new();
        let mut cached_query_answered = false;
        while let Some(req) = read_one_request(&mut s1, &mut buf).await {
            match req {
                ClientRequest::Hello { .. } => {
                    write_response(
                        &mut s1,
                        ResponsePayload::HelloAck {
                            server_version: WIRE_GUARD as u32,
                            caller_is_admin: false,
                        },
                    )
                    .await;
                }
                ClientRequest::Query {
                    query_id,
                    raw_query,
                    defaults,
                    sort_column,
                    sort_asc,
                    first_page_limit,
                } => {
                    if !cached_query_answered {
                        cached_query_answered = true;
                        write_response(
                            &mut s1,
                            ResponsePayload::QueryResult {
                                query_id,
                                session_id: 11,
                                total_matches: 1,
                                total_bytes: 32,
                                first_page: Vec::new(),
                            },
                        )
                        .await;
                        continue;
                    }
                    queries_srv.lock().await.push((
                        query_id,
                        raw_query,
                        defaults,
                        sort_column,
                        sort_asc,
                        first_page_limit,
                    ));
                    // Close before answering: the cached connection dies under the request.
                    break;
                }
                other => panic!("unexpected req on conn 1: {other:?}"),
            }
        }
        drop(s1);

        s2.connect().await.expect("accept 2");
        buf.clear();
        while let Some(req) = read_one_request(&mut s2, &mut buf).await {
            let reply = match req {
                ClientRequest::Hello { .. } => ResponsePayload::HelloAck {
                    server_version: WIRE_GUARD as u32,
                    caller_is_admin: false,
                },
                ClientRequest::Query {
                    query_id,
                    raw_query,
                    defaults,
                    sort_column,
                    sort_asc,
                    first_page_limit,
                } => {
                    queries_srv.lock().await.push((
                        query_id,
                        raw_query,
                        defaults,
                        sort_column,
                        sort_asc,
                        first_page_limit,
                    ));
                    ResponsePayload::QueryResult {
                        query_id,
                        session_id: 12,
                        total_matches: 1,
                        total_bytes: 64,
                        first_page: Vec::new(),
                    }
                }
                other => panic!("unexpected req on conn 2: {other:?}"),
            };
            write_response(&mut s2, reply).await;
        }
    });

    let session = ServiceSession::for_pipe(pipe);
    let warm = session
        .query(
            8,
            "warm",
            QueryDefaults::default(),
            SortColumn::Name,
            true,
            10,
        )
        .await
        .expect("the first query caches the primary connection");
    assert!(matches!(
        warm.payload,
        ResponsePayload::QueryResult { session_id: 11, .. }
    ));
    let query = session
        .query(
            9,
            "notes",
            QueryDefaults::default(),
            SortColumn::Size,
            false,
            25,
        )
        .await
        .expect("the retried query answers on the new connection");
    assert!(matches!(
        query.payload,
        ResponsePayload::QueryResult { session_id: 12, .. }
    ));
    drop(session);

    server_task.await.expect("server finish");
    let seen = queries.lock().await;
    assert_eq!(
        seen.len(),
        2,
        "the request is sent once per connection attempt"
    );
    assert_eq!(seen[0], seen[1], "the retry carries the same query fields");
}

#[tokio::test]
async fn cancel_uses_independent_connection_while_query_is_in_flight() {
    let pipe = unique_pipe_name("cancel_split");
    let s1 = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&pipe)
        .expect("primary pipe");

    let (query_arrived_tx, query_arrived_rx) = oneshot::channel::<()>();
    let (release_query_tx, release_query_rx) = oneshot::channel::<()>();
    let release_rx = Arc::new(Mutex::new(Some(release_query_rx)));
    let pipe_srv = pipe.clone();

    let primary_task = tokio::spawn(async move {
        let mut s1 = s1;
        s1.connect().await.expect("accept primary");
        let s2 = ServerOptions::new()
            .first_pipe_instance(false)
            .create(&pipe_srv)
            .expect("cancel pipe");

        let cancel_task = tokio::spawn(async move {
            let mut s2 = s2;
            s2.connect().await.expect("accept cancel");
            let mut buf = Vec::new();
            while let Some(req) = read_one_request(&mut s2, &mut buf).await {
                match req {
                    ClientRequest::Hello { .. } => {
                        write_response(
                            &mut s2,
                            ResponsePayload::HelloAck {
                                server_version: WIRE_GUARD as u32,
                                caller_is_admin: false,
                            },
                        )
                        .await;
                    }
                    ClientRequest::Cancel { query_id } => {
                        assert_eq!(query_id, 42);
                        write_response(&mut s2, ResponsePayload::CancelAck).await;
                        break;
                    }
                    other => panic!("unexpected req on cancel pipe: {other:?}"),
                }
            }
        });

        let mut buf = Vec::new();
        let mut query_arrived_tx = Some(query_arrived_tx);
        while let Some(req) = read_one_request(&mut s1, &mut buf).await {
            match req {
                ClientRequest::Hello { .. } => {
                    write_response(
                        &mut s1,
                        ResponsePayload::HelloAck {
                            server_version: WIRE_GUARD as u32,
                            caller_is_admin: false,
                        },
                    )
                    .await;
                }
                ClientRequest::Query { query_id, .. } => {
                    if let Some(tx) = query_arrived_tx.take() {
                        let _ = tx.send(());
                    }
                    if let Some(rx) = release_rx.lock().await.take() {
                        let _ = rx.await;
                    }
                    write_response(
                        &mut s1,
                        ResponsePayload::QueryResult {
                            query_id,
                            session_id: 1,
                            total_matches: 0,
                            total_bytes: 0,
                            first_page: Vec::new(),
                        },
                    )
                    .await;
                    break;
                }
                other => panic!("unexpected req on primary pipe: {other:?}"),
            }
        }
        cancel_task.await.expect("cancel server task");
    });

    let session = ServiceSession::for_pipe(pipe);
    let q_session = session.clone();
    let query_fut = tokio::spawn(async move {
        q_session
            .query(
                42,
                "slow",
                QueryDefaults::default(),
                SortColumn::Name,
                true,
                200,
            )
            .await
    });

    query_arrived_rx.await.expect("query reached server");
    let cancel_res = tokio::time::timeout(Duration::from_secs(2), session.cancel(42))
        .await
        .expect("cancel must not deadlock behind active query")
        .expect("cancel succeeds");
    assert!(matches!(cancel_res.payload, ResponsePayload::CancelAck));

    let _ = release_query_tx.send(());
    query_fut.await.expect("query join").expect("query ok");
    primary_task.await.expect("server join");
}

#[tokio::test]
async fn timed_out_primary_evicts_poisoned_connection() {
    let pipe = unique_pipe_name("timeout_evict");
    let s1 = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&pipe)
        .expect("first server");

    let accepts = Arc::new(AtomicUsize::new(0));
    let accepts_srv = accepts.clone();
    let pipe_srv = pipe.clone();

    let server_task = tokio::spawn(async move {
        let mut s1 = s1;
        s1.connect().await.expect("accept 1");
        accepts_srv.fetch_add(1, Ordering::SeqCst);
        let mut s2 = ServerOptions::new()
            .first_pipe_instance(false)
            .create(&pipe_srv)
            .expect("second server");

        let mut buf = Vec::new();
        while let Some(req) = read_one_request(&mut s1, &mut buf).await {
            match req {
                ClientRequest::Hello { .. } => {
                    write_response(
                        &mut s1,
                        ResponsePayload::HelloAck {
                            server_version: WIRE_GUARD as u32,
                            caller_is_admin: false,
                        },
                    )
                    .await;
                }
                ClientRequest::GetStatus => {
                    // Hang longer than client's timeout, then never reuse this conn.
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    break;
                }
                other => panic!("stale connection reused: {other:?}"),
            }
        }
        drop(s1);

        s2.connect().await.expect("accept 2");
        accepts_srv.fetch_add(1, Ordering::SeqCst);
        buf.clear();
        while let Some(req) = read_one_request(&mut s2, &mut buf).await {
            let reply = match req {
                ClientRequest::Hello { .. } => ResponsePayload::HelloAck {
                    server_version: WIRE_GUARD as u32,
                    caller_is_admin: false,
                },
                ClientRequest::GetStatus => ResponsePayload::StatusAck,
                other => panic!("unexpected req on conn 2: {other:?}"),
            };
            write_response(&mut s2, reply).await;
        }
    });

    let session = ServiceSession::for_pipe(pipe).with_read_timeout(Duration::from_millis(50));
    let err = session.get_status().await.expect_err("first must time out");
    assert!(matches!(err, ClientError::Timeout));

    session
        .get_status()
        .await
        .expect("next call must reconnect on a fresh pipe");
    drop(session);

    server_task.await.expect("server join");
    assert_eq!(accepts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn oversized_frame_shrinks_read_buf_back_to_baseline_and_reuses_write_buf() {
    let pipe = unique_pipe_name("buf_shrink");
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&pipe)
        .expect("server");

    let huge_text = "x".repeat(2 * 1024 * 1024);
    let server_task = tokio::spawn(async move {
        server.connect().await.expect("accept");
        let mut buf = Vec::new();
        while let Some(req) = read_one_request(&mut server, &mut buf).await {
            let reply = match req {
                ClientRequest::Hello { .. } => ResponsePayload::HelloAck {
                    server_version: WIRE_GUARD as u32,
                    caller_is_admin: false,
                },
                ClientRequest::GetConfig => ResponsePayload::ConfigResult {
                    config_toml: huge_text.clone(),
                },
                ClientRequest::SetConfig { config_toml } => ResponsePayload::SetConfigAck {
                    success: !config_toml.is_empty(),
                    message: String::new(),
                },
                ClientRequest::GetStatus => ResponsePayload::StatusAck,
                other => panic!("unexpected request: {other:?}"),
            };
            write_response(&mut server, reply).await;
        }
    });

    let mut client = CairnClient::connect_to(&pipe).await.expect("connect");
    let write_cap_initial = client.write_buf_capacity();
    let config_env = client.get_config().await.expect("huge config frame");
    assert!(matches!(
        config_env.payload,
        ResponsePayload::ConfigResult { ref config_toml } if config_toml.len() == 2 * 1024 * 1024
    ));
    assert!(
        client.read_buf_capacity() <= 65_536,
        "read_buf should shrink back to 64 KiB baseline, got {}",
        client.read_buf_capacity()
    );

    // Oversized write should also return write_buf to its baseline.
    let (ok, _) = client
        .submit_service_config(&"y".repeat(256 * 1024))
        .await
        .expect("large set_config");
    assert!(ok);
    assert!(
        client.write_buf_capacity() <= 65_536,
        "write_buf should shrink back to baseline after oversized write, got {}",
        client.write_buf_capacity()
    );

    client.get_status().await.expect("status");
    assert!(client.write_buf_capacity() >= write_cap_initial);

    drop(client);
    server_task.await.expect("server join");
}

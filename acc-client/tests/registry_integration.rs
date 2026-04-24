//! Integration tests for project + agent registry endpoints.

use acc_client::Client;
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn client_for(server: &MockServer) -> Client {
    Client::new(server.uri(), "t").unwrap()
}

#[tokio::test]
async fn projects_list_handles_wrapped_envelope_with_total() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "projects": [{"id": "proj-1", "name": "demo", "status": "active"}],
            "total": 1,
            "offset": 0
        })))
        .mount(&server)
        .await;
    let client = client_for(&server).await;
    let v = client.projects().list().send().await.unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].name, "demo");
}

#[tokio::test]
async fn project_create_handles_ok_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "project": {"id": "proj-2", "name": "new", "status": "active"}
        })))
        .mount(&server)
        .await;
    let client = client_for(&server).await;
    let p = client
        .projects()
        .create(&acc_client::model::CreateProjectRequest {
            name: "new".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(p.id, "proj-2");
}

#[tokio::test]
async fn agents_list_filters_by_online() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/agents"))
        .and(query_param("online", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "agents": [
                {"name": "natasha", "online": true, "onlineStatus": "online", "gpu": true, "gpu_temp_c": 48.0},
                {"name": "boris",   "online": true, "onlineStatus": "online"}
            ]
        })))
        .mount(&server)
        .await;
    let client = client_for(&server).await;
    let agents = client.agents().list().online(true).send().await.unwrap();
    assert_eq!(agents.len(), 2);
    // GPU telemetry rode in via `extra`
    let natasha = agents.iter().find(|a| a.name == "natasha").unwrap();
    assert!(natasha.extra.contains_key("gpu_temp_c"));
}

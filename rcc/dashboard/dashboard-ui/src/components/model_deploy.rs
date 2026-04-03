/// model_deploy.rs — Model hot-swap deployment card
///
/// Shows current model on each Sweden node, and lets jkh trigger a fleet
/// model deployment via POST /api/models/deploy.

use leptos::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeModelInfo {
    pub agent: String,
    pub port: u16,
    pub status: String,
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CurrentModelsResp {
    pub nodes: Vec<NodeModelInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployResp {
    pub ok: Option<bool>,
    pub deploy_id: Option<String>,
    pub model_id: Option<String>,
    pub agents: Option<Vec<String>>,
    pub dry_run: Option<bool>,
    pub status: Option<String>,
    pub log_path: Option<String>,
    pub message: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployStatus {
    pub deploy_id: String,
    pub status: String,
    pub log_lines: Option<u32>,
    pub log_tail: Vec<String>,
    pub log_path: String,
}

async fn fetch_current_models() -> Result<CurrentModelsResp, String> {
    let resp = gloo_net::http::Request::get("/api/models/current")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    resp.json::<CurrentModelsResp>().await.map_err(|e| e.to_string())
}

async fn trigger_deploy(model_id: String, agents: Vec<String>, dry_run: bool) -> Result<DeployResp, String> {
    let body = serde_json::json!({
        "model_id": model_id,
        "agents": agents,
        "dry_run": dry_run,
    });
    let resp = gloo_net::http::Request::post("/api/models/deploy")
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.json::<DeployResp>().await.map_err(|e| e.to_string())
}

async fn poll_deploy_status(deploy_id: String) -> Result<DeployStatus, String> {
    let resp = gloo_net::http::Request::get(&format!("/api/models/deploy/{}", deploy_id))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.json::<DeployStatus>().await.map_err(|e| e.to_string())
}

#[component]
pub fn ModelDeploy() -> impl IntoView {
    // Current node models (auto-refreshed)
    let (refresh_tick, set_refresh_tick) = create_signal(0u32);
    let current_models = create_resource(
        move || refresh_tick.get(),
        |_| async { fetch_current_models().await },
    );

    // Deploy form state
    let (model_input, set_model_input) = create_signal("google/gemma-4-31B-it".to_string());
    let (dry_run, set_dry_run) = create_signal(false);
    let (selected_agents, set_selected_agents) = create_signal(
        vec!["boris", "peabody", "sherman", "snidely", "dudley"]
            .into_iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
    );
    let (deploy_error, set_deploy_error) = create_signal::<Option<String>>(None);
    let (deploy_id, set_deploy_id) = create_signal::<Option<String>>(None);
    let (deploy_status, set_deploy_status) = create_signal::<Option<DeployStatus>>(None);
    let (deploying, set_deploying) = create_signal(false);

    // Poll status when we have a deploy_id
    let status_resource = create_resource(
        move || deploy_id.get(),
        |id| async move {
            if let Some(id) = id {
                poll_deploy_status(id).await.ok()
            } else {
                None
            }
        },
    );

    // Update status signal from resource
    create_effect(move |_| {
        if let Some(Some(st)) = status_resource.get() {
            set_deploy_status.set(Some(st.clone()));
            // If terminal, stop deploying spinner
            if matches!(st.status.as_str(), "succeeded" | "failed" | "validated" | "partial_success") {
                set_deploying.set(false);
            }
        }
    });

    let all_agents = vec!["boris", "peabody", "sherman", "snidely", "dudley"];

    let toggle_agent = move |name: &'static str| {
        set_selected_agents.update(|v| {
            if v.contains(&name.to_string()) {
                v.retain(|a| a != name);
            } else {
                v.push(name.to_string());
            }
        });
    };

    let on_deploy = move |ev: leptos::ev::MouseEvent| {
        ev.prevent_default();
        let model = model_input.get();
        if model.trim().is_empty() {
            set_deploy_error.set(Some("Model ID is required".to_string()));
            return;
        }
        let agents = selected_agents.get();
        if agents.is_empty() {
            set_deploy_error.set(Some("Select at least one agent".to_string()));
            return;
        }
        set_deploy_error.set(None);
        set_deploy_id.set(None);
        set_deploy_status.set(None);
        set_deploying.set(true);

        let dr = dry_run.get();
        spawn_local(async move {
            match trigger_deploy(model, agents, dr).await {
                Ok(resp) => {
                    if let Some(id) = resp.deploy_id {
                        set_deploy_id.set(Some(id));
                    } else if let Some(err) = resp.error {
                        set_deploy_error.set(Some(err));
                        set_deploying.set(false);
                    }
                }
                Err(e) => {
                    set_deploy_error.set(Some(e));
                    set_deploying.set(false);
                }
            }
        });
    };

    view! {
        <div class="model-deploy-panel">
            <div class="panel-header">
                <h2>"🚀 Model Deployment"</h2>
                <button
                    class="refresh-btn"
                    on:click=move |_| set_refresh_tick.update(|t| *t += 1)
                >"↻ Refresh"</button>
            </div>

            // Current model status table
            <section class="section-block">
                <h3>"Current Fleet Models"</h3>
                {move || match current_models.get() {
                    None => view! { <p class="loading">"Loading..."</p> }.into_view(),
                    Some(Err(e)) => view! { <p class="error">{format!("Error: {}", e)}</p> }.into_view(),
                    Some(Ok(data)) => view! {
                        <table class="model-table">
                            <thead>
                                <tr>
                                    <th>"Agent"</th>
                                    <th>"Status"</th>
                                    <th>"Model(s)"</th>
                                </tr>
                            </thead>
                            <tbody>
                                {data.nodes.iter().map(|node| {
                                    let status_class = if node.status == "ok" { "status-ok" } else { "status-err" };
                                    let models_str = if node.models.is_empty() {
                                        "—".to_string()
                                    } else {
                                        node.models.join(", ")
                                    };
                                    view! {
                                        <tr>
                                            <td class="agent-name">{node.agent.clone()}</td>
                                            <td class={status_class}>{node.status.clone()}</td>
                                            <td class="model-name">{models_str}</td>
                                        </tr>
                                    }
                                }).collect::<Vec<_>>()}
                            </tbody>
                        </table>
                    }.into_view(),
                }}
            </section>

            // Deploy form
            <section class="section-block deploy-form">
                <h3>"Deploy New Model"</h3>

                <div class="form-row">
                    <label>"HuggingFace Model ID"</label>
                    <input
                        type="text"
                        class="model-input"
                        placeholder="google/gemma-4-31B-it"
                        prop:value=move || model_input.get()
                        on:input=move |ev| set_model_input.set(event_target_value(&ev))
                    />
                </div>

                <div class="form-row">
                    <label>"Target Agents"</label>
                    <div class="agent-toggles">
                        {all_agents.iter().map(|&name| {
                            let name_str = name.to_string();
                            view! {
                                <label class="agent-toggle">
                                    <input
                                        type="checkbox"
                                        checked=move || selected_agents.get().contains(&name_str)
                                        on:change=move |_| toggle_agent(name)
                                    />
                                    {name}
                                </label>
                            }
                        }).collect::<Vec<_>>()}
                    </div>
                </div>

                <div class="form-row">
                    <label class="dry-run-label">
                        <input
                            type="checkbox"
                            prop:checked=move || dry_run.get()
                            on:change=move |ev| set_dry_run.set(event_target_checked(&ev))
                        />
                        " Dry run (validate only — no restart)"
                    </label>
                </div>

                {move || deploy_error.get().map(|e| view! {
                    <p class="error">{"⚠ "}{e}</p>
                })}

                <button
                    class="deploy-btn"
                    class:deploying=move || deploying.get()
                    disabled=move || deploying.get()
                    on:click=on_deploy
                >
                    {move || if deploying.get() {
                        "⏳ Deploying..."
                    } else if dry_run.get() {
                        "🔍 Validate Model"
                    } else {
                        "🚀 Deploy to Fleet"
                    }}
                </button>
            </section>

            // Deploy status
            {move || deploy_status.get().map(|st| view! {
                <section class="section-block deploy-status">
                    <h3>"Deploy Status — " {st.deploy_id.clone()}</h3>
                    <p class={if st.status == "succeeded" { "status-ok" } else if st.status == "failed" { "status-err" } else { "status-running" }}>
                        {"Status: "} {st.status.clone()}
                    </p>
                    <div class="deploy-log">
                        <pre>
                            {st.log_tail.join("\n")}
                        </pre>
                    </div>
                    {if !matches!(st.status.as_str(), "succeeded" | "failed" | "validated") {
                        view! {
                            <button
                                class="refresh-btn"
                                on:click=move |_| { status_resource.refetch(); }
                            >"↻ Refresh Status"</button>
                        }.into_view()
                    } else {
                        view! { <p class="muted">"Log: " {st.log_path.clone()}</p> }.into_view()
                    }}
                </section>
            })}
        </div>
    }
}

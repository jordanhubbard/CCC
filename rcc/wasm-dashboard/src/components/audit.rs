use leptos::prelude::*;
use leptos::task::spawn_local;
use gloo_timers::callback::Interval;
use crate::types::CapEvent;
use crate::api;

// ── Audit tab — cap_audit_log live visualization ───────────────────────────
//
// Shows GRANT/REVOKE/ATTENUATION/DENY events from the agentOS cap_audit_log
// ring buffer (cap_audit_log.c, commit 1cfffeb).  Polls GET /api/agentos/cap-events
// on a 5-second interval.  Supports filter-by-slot and filter-by-event-type.
// Export button triggers a JSON download via ?export=1.

#[component]
pub fn AuditTab() -> impl IntoView {
    let events:      RwSignal<Vec<CapEvent>> = RwSignal::new(vec![]);
    let total_ring:  RwSignal<u64>           = RwSignal::new(0);
    let slots_list:  RwSignal<Vec<u32>>      = RwSignal::new(vec![]);
    let types_list:  RwSignal<Vec<String>>   = RwSignal::new(vec![]);
    let caps_list:   RwSignal<Vec<String>>   = RwSignal::new(vec![]);
    let error:       RwSignal<Option<String>>= RwSignal::new(None);

    let filter_slot: RwSignal<String> = RwSignal::new("all".into());
    let filter_type: RwSignal<String> = RwSignal::new("all".into());

    // Initial load
    {
        let (ev, tr, sl, tl, cl, er) = (events, total_ring, slots_list, types_list, caps_list, error);
        spawn_local(async move {
            match api::fetch_cap_events(200, None, None).await {
                Ok(resp) => {
                    ev.set(resp.events);
                    tr.set(resp.total_in_ring);
                    sl.set(resp.slots);
                    tl.set(resp.event_types);
                    cl.set(resp.cap_classes);
                }
                Err(e) => { er.set(Some(e)); }
            }
        });
    }

    // Poll every 5s
    {
        let (ev, tr, er, ft, fs) = (events, total_ring, error, filter_type, filter_slot);
        let _iv = Interval::new(5_000, move || {
            let (ev2, tr2, er2, ft2, fs2) = (ev, tr, er, ft, fs);
            spawn_local(async move {
                let slot_f  = fs2.get();
                let type_f  = ft2.get();
                let slot    = if slot_f  == "all" { None } else { slot_f.parse::<u32>().ok() };
                let ev_type = if type_f  == "all" { None } else { Some(type_f.as_str()) };
                match api::fetch_cap_events(200, slot, ev_type).await {
                    Ok(resp) => {
                        ev2.set(resp.events);
                        tr2.set(resp.total_in_ring);
                        er2.set(None);
                    }
                    Err(e) => { er2.set(Some(e)); }
                }
            });
        });
        _iv.forget();
    }

    // Filtered view (applied client-side too for instant filter response)
    let filtered = move || {
        let fs = filter_slot.get();
        let ft = filter_type.get();
        events.get()
            .into_iter()
            .filter(|e| {
                (fs == "all" || e.slot_id.to_string() == fs)
                && (ft == "all" || e.event_type.to_uppercase() == ft.to_uppercase())
            })
            .collect::<Vec<_>>()
    };

    let export_url = move || {
        let fs = filter_slot.get();
        let ft = filter_type.get();
        let mut url = "/api/agentos/cap-events?export=1&limit=500".to_string();
        if fs != "all" { url.push_str(&format!("&slot={}", fs)); }
        if ft != "all" { url.push_str(&format!("&type={}", ft)); }
        url
    };

    view! {
        <div class="audit-tab">
            <div class="audit-header">
                <h2>"🔍 Capability Audit Log"</h2>
                <span class="audit-ring-stat">
                    "Ring: " {move || total_ring.get()} " events"
                </span>
            </div>

            // Filter bar
            <div class="audit-filters">
                <label>"Slot:"
                    <select on:change=move |ev| filter_slot.set(event_target_value(&ev))>
                        <option value="all">"All slots"</option>
                        {move || slots_list.get().into_iter().map(|s| view! {
                            <option value={s.to_string()}>{format!("Slot {}", s)}</option>
                        }).collect::<Vec<_>>()}
                    </select>
                </label>

                <label>"Event type:"
                    <select on:change=move |ev| filter_type.set(event_target_value(&ev))>
                        <option value="all">"All types"</option>
                        {move || types_list.get().into_iter().map(|t| {
                            let tl_val  = t.clone();
                            let tl_disp = t.clone();
                            view! { <option value={tl_val}>{tl_disp}</option> }
                        }).collect::<Vec<_>>()}
                    </select>
                </label>

                <a class="export-btn" href={export_url} download="cap-audit-export.json">
                    "⬇ Export JSON"
                </a>

                <span class="caps-legend">
                    "Caps: "
                    {move || caps_list.get().into_iter().map(|c| {
                        view! { <span class="cap-badge">{c}</span> }
                    }).collect::<Vec<_>>()}
                </span>
            </div>

            // Error
            {move || error.get().map(|e| view! {
                <div class="audit-error">"⚠ " {e}</div>
            })}

            // Table
            <div class="audit-table-wrap">
                <table class="audit-table">
                    <thead>
                        <tr>
                            <th>"Seq"</th>
                            <th>"Timestamp"</th>
                            <th>"Tick"</th>
                            <th>"Event"</th>
                            <th>"Slot"</th>
                            <th>"Agent ID"</th>
                            <th>"Caps"</th>
                            <th>"Mask"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {move || filtered().into_iter().map(|e| {
                            let ts_s  = e.ts / 1000;
                            let ts_ms = e.ts % 1000;
                            let hh    = (ts_s % 86400) / 3600;
                            let mm    = (ts_s % 3600)  / 60;
                            let ss    = ts_s % 60;
                            let ts_str = format!("{:02}:{:02}:{:02}.{:03}", hh, mm, ss, ts_ms);
                            let ev_class = match e.event_type.as_str() {
                                "GRANT"       => "ev-grant",
                                "REVOKE"      => "ev-revoke",
                                "ATTENUATION" => "ev-attenuation",
                                "DENY"        => "ev-deny",
                                _             => "ev-unknown",
                            };
                            let caps_str = e.caps_names.join(", ");
                            view! {
                                <tr>
                                    <td class="mono">{e.seq.to_string()}</td>
                                    <td class="mono">{ts_str}</td>
                                    <td class="mono">{e.tick.clone()}</td>
                                    <td><span class={format!("ev-badge {}", ev_class)}>{e.event_type.clone()}</span></td>
                                    <td>{e.slot_id.to_string()}</td>
                                    <td class="mono">{format!("pid={}", e.agent_id)}</td>
                                    <td class="caps-cell">{caps_str}</td>
                                    <td class="mono">{e.caps_mask.clone()}</td>
                                </tr>
                            }
                        }).collect::<Vec<_>>()}
                    </tbody>
                </table>
            </div>

            <div class="audit-footer">
                "Showing " {move || filtered().len().to_string()} " / " {move || total_ring.get()} " ring events · auto-refresh 5s"
            </div>
        </div>
    }
}

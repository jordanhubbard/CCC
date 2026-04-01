use leptos::prelude::*;
use leptos::task::spawn_local;
use gloo_timers::callback::Interval;
use crate::types::{SlotProfile, ProfileFrame};
use crate::api;

// ── Profiler tab — live WASM slot flame graph ──────────────────────────────
//
// Polls GET /api/agentos/wasm-profiles every 2s.
// Shows a summary table (slot, name, cpu%, mem KB, ticks) and a per-slot
// SVG flame graph (depth-stacked bars, FNV-1a hashed hue, hover tooltips).

// ── FNV-1a 32-bit hue from function name ──────────────────────────────────
fn fnv1a_hue(s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    (h % 300 + 30) // hue 30-330, avoiding pure red (danger zone)
}

// ── SVG flame graph for one slot ──────────────────────────────────────────
#[component]
fn FlameGraph(profile: SlotProfile) -> impl IntoView {
    let width  = 480u32;
    let bar_h  = 22u32;
    let pad    = 2u32;
    let total_ticks = profile.ticks.max(1);

    // Sort frames: depth asc, then ticks desc
    let mut frames = profile.frames.clone();
    frames.sort_by(|a, b| a.depth.cmp(&b.depth).then(b.ticks.cmp(&a.ticks)));

    let max_depth = frames.iter().map(|f| f.depth).max().unwrap_or(0);
    let svg_h = (max_depth + 1) * (bar_h + pad) + pad;

    let tooltip: RwSignal<Option<String>> = RwSignal::new(None);

    let rects = frames.iter().map(|f| {
        let hue   = fnv1a_hue(&f.fn_name);
        let pct   = (f.ticks as f64 / total_ticks as f64).min(1.0);
        let bar_w = (pct * (width as f64 - 2.0 * pad as f64)) as u32;
        let x     = pad;
        let y     = f.depth * (bar_h + pad) + pad;
        let color = format!("hsl({hue},60%,45%)");
        let label = if bar_w > 60 {
            let max_chars = (bar_w / 7) as usize;
            let disp = if f.fn_name.len() > max_chars {
                format!("{}…", &f.fn_name[..max_chars.saturating_sub(1)])
            } else {
                f.fn_name.clone()
            };
            Some(disp)
        } else {
            None
        };
        let tip_text = format!("{} — {} ticks ({:.1}%)", f.fn_name, f.ticks, pct * 100.0);
        let tt = tooltip;

        view! {
            <g
                on:mouseenter=move |_| tt.set(Some(tip_text.clone()))
                on:mouseleave=move |_| tt.set(None)
            >
                <rect
                    x={x.to_string()} y={y.to_string()}
                    width={bar_w.to_string()} height={bar_h.to_string()}
                    fill={color} rx="3"
                    style="cursor:pointer;"
                />
                {label.map(|l| view! {
                    <text
                        x={(x + 4).to_string()}
                        y={(y + bar_h - 6).to_string()}
                        font-size="11" fill="rgba(255,255,255,0.9)"
                        font-family="monospace"
                        pointer-events="none"
                    >{l}</text>
                })}
            </g>
        }
    }).collect::<Vec<_>>();

    view! {
        <div class="flame-wrap">
            <div class="flame-title">
                <span class="slot-badge">{format!("Slot {}", profile.id)}</span>
                <strong>{profile.name.clone()}</strong>
                <span class="flame-cpu">{format!("{}% CPU", profile.cpu_pct)}</span>
                <span class="flame-mem">{format!("{} KB", profile.mem_kb)}</span>
            </div>
            <div class="flame-svg-wrap" style={format!("height:{}px", svg_h + 4)}>
                <svg
                    width={width.to_string()} height={svg_h.to_string()}
                    style="display:block;width:100%;max-width:480px"
                    viewBox={format!("0 0 {width} {svg_h}")}
                    xmlns="http://www.w3.org/2000/svg"
                >
                    {rects}
                </svg>
            </div>
            {move || tooltip.get().map(|t| view! {
                <div class="flame-tooltip">{t}</div>
            })}
        </div>
    }
}

// ── Profiler tab root ──────────────────────────────────────────────────────
#[component]
pub fn ProfilerTab() -> impl IntoView {
    let snapshot: RwSignal<Option<Vec<SlotProfile>>> = RwSignal::new(None);
    let error:    RwSignal<Option<String>>           = RwSignal::new(None);
    let auto_refresh: RwSignal<bool>                 = RwSignal::new(true);
    let last_ts:  RwSignal<u64>                      = RwSignal::new(0);

    // Initial load
    {
        let (sn, er, lt) = (snapshot, error, last_ts);
        spawn_local(async move {
            match api::fetch_wasm_profiles().await {
                Ok(p) => { sn.set(Some(p.slots)); lt.set(p.ts); }
                Err(e) => { er.set(Some(e)); }
            }
        });
    }

    // Auto-refresh every 2s
    {
        let (sn, er, lt, ar) = (snapshot, error, last_ts, auto_refresh);
        let _iv = Interval::new(2_000, move || {
            if !ar.get() { return; }
            let (sn2, er2, lt2) = (sn, er, lt);
            spawn_local(async move {
                match api::fetch_wasm_profiles().await {
                    Ok(p) => { sn2.set(Some(p.slots)); lt2.set(p.ts); er2.set(None); }
                    Err(e) => { er2.set(Some(e)); }
                }
            });
        });
        _iv.forget();
    }

    view! {
        <div class="profiler-tab">
            <div class="profiler-header">
                <h2>"🔥 WASM Slot Profiler"</h2>
                <div class="profiler-controls">
                    <label class="toggle-label">
                        "Auto-refresh"
                        <input
                            type="checkbox"
                            prop:checked={move || auto_refresh.get()}
                            on:change=move |_| auto_refresh.update(|v| *v = !*v)
                        />
                    </label>
                    <button
                        class="btn-sm"
                        on:click=move |_| {
                            let (sn, er, lt) = (snapshot, error, last_ts);
                            spawn_local(async move {
                                match api::fetch_wasm_profiles().await {
                                    Ok(p) => { sn.set(Some(p.slots)); lt.set(p.ts); er.set(None); }
                                    Err(e) => { er.set(Some(e)); }
                                }
                            });
                        }
                    >"📸 Snapshot"</button>
                    <span class="profiler-ts">
                        {move || {
                            let ts = last_ts.get();
                            if ts == 0 { "—".to_string() }
                            else {
                                let s = ts / 1000;
                                let hh = (s % 86400) / 3600;
                                let mm = (s % 3600) / 60;
                                let ss = s % 60;
                                format!("Snapshot {hh:02}:{mm:02}:{ss:02}")
                            }
                        }}
                    </span>
                </div>
            </div>

            {move || error.get().map(|e| view! {
                <div class="profiler-error">"⚠ " {e}</div>
            })}

            {move || snapshot.get().map(|slots| {
                // Summary table
                let table = view! {
                    <table class="profiler-table">
                        <thead><tr>
                            <th>"Slot"</th><th>"Name"</th>
                            <th>"CPU %"</th><th>"Mem KB"</th>
                            <th>"Ticks"</th><th>"Top Function"</th>
                        </tr></thead>
                        <tbody>
                            {slots.iter().map(|s| {
                                let top_fn = s.frames.iter()
                                    .filter(|f| f.depth == 0)
                                    .max_by_key(|f| f.ticks)
                                    .map(|f| f.fn_name.clone())
                                    .unwrap_or_else(|| "—".to_string());
                                let cpu_class = if s.cpu_pct > 80 { "cpu-high" }
                                    else if s.cpu_pct > 40 { "cpu-mid" }
                                    else { "cpu-low" };
                                view! {
                                    <tr>
                                        <td>{s.id.to_string()}</td>
                                        <td class="mono">{s.name.clone()}</td>
                                        <td><span class={format!("cpu-badge {}", cpu_class)}>{format!("{}%", s.cpu_pct)}</span></td>
                                        <td>{s.mem_kb.to_string()}</td>
                                        <td class="mono">{s.ticks.to_string()}</td>
                                        <td class="mono">{top_fn}</td>
                                    </tr>
                                }
                            }).collect::<Vec<_>>()}
                        </tbody>
                    </table>
                };

                // Flame graphs
                let flames = slots.iter().map(|s| {
                    view! { <FlameGraph profile={s.clone()} /> }
                }).collect::<Vec<_>>();

                view! {
                    <div>
                        {table}
                        <div class="flame-grid">{flames}</div>
                    </div>
                }
            })}

            <div class="profiler-footer">
                "Data: " {move || {
                    if snapshot.get().is_none() { "loading…".to_string() }
                    else { format!("{} slots · auto-refresh 2s · hover frames for details",
                        snapshot.get().map(|s| s.len()).unwrap_or(0)) }
                }}
            </div>
        </div>
    }
}

/// DiffView — unified & side-by-side diff renderer for the Coding Agent tab
///
/// Parses standard unified-diff output (diff --git, ---, +++, @@ hunks)
/// and renders it with line numbers, color-coded add/del/ctx lines,
/// hunk navigation, and a toggle between unified and side-by-side modes.

use leptos::*;

// ── Diff parser types ──────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct DiffFile {
    pub old_name: String,
    pub new_name: String,
    pub hunks: Vec<DiffHunk>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiffHunk {
    pub header: String,
    pub old_start: u32,
    pub new_start: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DiffLineKind {
    Context,
    Add,
    Del,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub content: String,
    pub old_num: Option<u32>,
    pub new_num: Option<u32>,
}

// ── Parser ─────────────────────────────────────────────────────────────────

fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    // @@ -old_start,old_count +new_start,new_count @@
    let rest = line.strip_prefix("@@ -")?;
    let parts: Vec<&str> = rest.splitn(2, " +").collect();
    if parts.len() < 2 {
        return None;
    }
    let old_start: u32 = parts[0].split(',').next()?.parse().ok()?;
    let new_part = parts[1].split(" @@").next()?;
    let new_start: u32 = new_part.split(',').next()?.parse().ok()?;
    Some((old_start, new_start))
}

pub fn parse_diff(text: &str) -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        // Look for file header
        if lines[i].starts_with("diff --git") || lines[i].starts_with("--- ") {
            let mut old_name = String::new();
            let mut new_name = String::new();

            // Skip "diff --git a/X b/Y" line
            if lines[i].starts_with("diff --git") {
                let parts: Vec<&str> = lines[i].splitn(4, ' ').collect();
                if parts.len() >= 4 {
                    old_name = parts[2].strip_prefix("a/").unwrap_or(parts[2]).to_string();
                    new_name = parts[3].strip_prefix("b/").unwrap_or(parts[3]).to_string();
                }
                i += 1;
                // Skip index, mode lines
                while i < lines.len()
                    && !lines[i].starts_with("--- ")
                    && !lines[i].starts_with("diff --git")
                    && !lines[i].starts_with("@@ ")
                {
                    i += 1;
                }
            }

            // --- a/file
            if i < lines.len() && lines[i].starts_with("--- ") {
                let name = lines[i][4..].trim();
                if old_name.is_empty() {
                    old_name = name.strip_prefix("a/").unwrap_or(name).to_string();
                }
                i += 1;
            }

            // +++ b/file
            if i < lines.len() && lines[i].starts_with("+++ ") {
                let name = lines[i][4..].trim();
                if new_name.is_empty() {
                    new_name = name.strip_prefix("b/").unwrap_or(name).to_string();
                }
                i += 1;
            }

            // Parse hunks
            let mut hunks: Vec<DiffHunk> = Vec::new();
            while i < lines.len() && !lines[i].starts_with("diff --git") {
                if lines[i].starts_with("@@ ") {
                    let header = lines[i].to_string();
                    let (old_start, new_start) =
                        parse_hunk_header(lines[i]).unwrap_or((1, 1));
                    i += 1;

                    let mut hunk_lines: Vec<DiffLine> = Vec::new();
                    let mut old_num = old_start;
                    let mut new_num = new_start;

                    while i < lines.len()
                        && !lines[i].starts_with("@@ ")
                        && !lines[i].starts_with("diff --git")
                    {
                        let line = lines[i];
                        if let Some(content) = line.strip_prefix('+') {
                            hunk_lines.push(DiffLine {
                                kind: DiffLineKind::Add,
                                content: content.to_string(),
                                old_num: None,
                                new_num: Some(new_num),
                            });
                            new_num += 1;
                        } else if let Some(content) = line.strip_prefix('-') {
                            hunk_lines.push(DiffLine {
                                kind: DiffLineKind::Del,
                                content: content.to_string(),
                                old_num: Some(old_num),
                                new_num: None,
                            });
                            old_num += 1;
                        } else if line.starts_with(' ') || line.is_empty() {
                            let content = if line.is_empty() {
                                String::new()
                            } else {
                                line[1..].to_string()
                            };
                            hunk_lines.push(DiffLine {
                                kind: DiffLineKind::Context,
                                content,
                                old_num: Some(old_num),
                                new_num: Some(new_num),
                            });
                            old_num += 1;
                            new_num += 1;
                        } else if line.starts_with("\\ No newline") {
                            // skip
                        } else {
                            // Unknown line in hunk — treat as context
                            hunk_lines.push(DiffLine {
                                kind: DiffLineKind::Context,
                                content: line.to_string(),
                                old_num: Some(old_num),
                                new_num: Some(new_num),
                            });
                            old_num += 1;
                            new_num += 1;
                        }
                        i += 1;
                    }

                    hunks.push(DiffHunk {
                        header,
                        old_start,
                        new_start,
                        lines: hunk_lines,
                    });
                } else {
                    i += 1;
                }
            }

            files.push(DiffFile {
                old_name,
                new_name,
                hunks,
            });
        } else {
            i += 1;
        }
    }

    files
}

/// Detect if text contains unified diff content
pub fn looks_like_diff(text: &str) -> bool {
    let has_diff_header = text.contains("diff --git");
    let has_hunk = text.contains("\n@@ ");
    let has_plus_minus = text.contains("\n--- ") && text.contains("\n+++ ");
    (has_diff_header && has_hunk) || (has_plus_minus && has_hunk)
}

/// Extract diff portion from mixed output
pub fn extract_diff(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut result = Vec::new();
    let mut in_diff = false;

    for line in &lines {
        if line.starts_with("diff --git") {
            in_diff = true;
        }
        if in_diff {
            result.push(*line);
            // End of diff: blank line after a hunk, or non-diff line
            // Actually, keep going until we hit something clearly not a diff
        }
    }

    if result.is_empty() {
        // Try finding --- / +++ pairs
        let mut i = 0;
        while i < lines.len() {
            if lines[i].starts_with("--- ") && i + 1 < lines.len() && lines[i + 1].starts_with("+++ ") {
                // Found diff start, scan back for possible diff --git
                let start = if i > 0 && lines[i - 1].starts_with("diff --git") {
                    i - 1
                } else {
                    i
                };
                let mut j = i + 2;
                while j < lines.len()
                    && (lines[j].starts_with("@@ ")
                        || lines[j].starts_with('+')
                        || lines[j].starts_with('-')
                        || lines[j].starts_with(' ')
                        || lines[j].starts_with("\\ ")
                        || lines[j].is_empty())
                {
                    j += 1;
                }
                for k in start..j {
                    result.push(lines[k]);
                }
            }
            i += 1;
        }
    }

    result.join("\n")
}

// ── Side-by-side pairing ───────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct SideBySidePair {
    pub left: Option<DiffLine>,
    pub right: Option<DiffLine>,
}

fn pair_lines(lines: &[DiffLine]) -> Vec<SideBySidePair> {
    let mut pairs: Vec<SideBySidePair> = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        match lines[i].kind {
            DiffLineKind::Context => {
                pairs.push(SideBySidePair {
                    left: Some(lines[i].clone()),
                    right: Some(lines[i].clone()),
                });
                i += 1;
            }
            DiffLineKind::Del => {
                // Collect consecutive deletions
                let del_start = i;
                while i < lines.len() && lines[i].kind == DiffLineKind::Del {
                    i += 1;
                }
                let del_end = i;

                // Collect consecutive additions that follow
                let add_start = i;
                while i < lines.len() && lines[i].kind == DiffLineKind::Add {
                    i += 1;
                }
                let add_end = i;

                let del_count = del_end - del_start;
                let add_count = add_end - add_start;
                let max = del_count.max(add_count);

                for j in 0..max {
                    pairs.push(SideBySidePair {
                        left: if j < del_count {
                            Some(lines[del_start + j].clone())
                        } else {
                            None
                        },
                        right: if j < add_count {
                            Some(lines[add_start + j].clone())
                        } else {
                            None
                        },
                    });
                }
            }
            DiffLineKind::Add => {
                // Lone additions (no preceding deletions)
                pairs.push(SideBySidePair {
                    left: None,
                    right: Some(lines[i].clone()),
                });
                i += 1;
            }
        }
    }

    pairs
}

// ── Components ─────────────────────────────────────────────────────────────

#[component]
pub fn DiffView(
    #[prop(into)] diff_text: String,
) -> impl IntoView {
    let files = parse_diff(&diff_text);
    let mode = create_rw_signal::<String>("unified".to_string());
    let collapsed = create_rw_signal::<Vec<(usize, usize)>>(vec![]);

    // Stats
    let total_add: usize = files
        .iter()
        .flat_map(|f| f.hunks.iter())
        .flat_map(|h| h.lines.iter())
        .filter(|l| l.kind == DiffLineKind::Add)
        .count();
    let total_del: usize = files
        .iter()
        .flat_map(|f| f.hunks.iter())
        .flat_map(|h| h.lines.iter())
        .filter(|l| l.kind == DiffLineKind::Del)
        .count();
    let file_count = files.len();

    if files.is_empty() {
        return view! {
            <div class="diff-empty">"No diff content to display"</div>
        }
        .into_view();
    }

    let files_for_nav = files.clone();
    let files_for_body = files.clone();

    view! {
        <div class="diff-view">
            // Header bar
            <div class="diff-toolbar">
                <div class="diff-stats">
                    <span class="diff-stat-files">{file_count}" file"{if file_count != 1 { "s" } else { "" }}</span>
                    <span class="diff-stat-add">"+"{ total_add }</span>
                    <span class="diff-stat-del">"-"{ total_del }</span>
                </div>
                <div class="diff-mode-toggle">
                    <button
                        class="diff-mode-btn"
                        class:diff-mode-active=move || mode.get() == "unified"
                        on:click={
                            let mode = mode.clone();
                            move |_| mode.set("unified".to_string())
                        }
                    >"Unified"</button>
                    <button
                        class="diff-mode-btn"
                        class:diff-mode-active=move || mode.get() == "split"
                        on:click={
                            let mode = mode.clone();
                            move |_| mode.set("split".to_string())
                        }
                    >"Side-by-Side"</button>
                </div>
            </div>

            // File navigation (jump links)
            {if file_count > 1 {
                view! {
                    <div class="diff-file-nav">
                        {files_for_nav.iter().enumerate().map(|(fi, f)| {
                            let name = f.new_name.clone();
                            let add_count: usize = f.hunks.iter()
                                .flat_map(|h| h.lines.iter())
                                .filter(|l| l.kind == DiffLineKind::Add)
                                .count();
                            let del_count: usize = f.hunks.iter()
                                .flat_map(|h| h.lines.iter())
                                .filter(|l| l.kind == DiffLineKind::Del)
                                .count();
                            let anchor = format!("diff-file-{fi}");
                            view! {
                                <a class="diff-nav-item" href={format!("#{anchor}")}>
                                    <span class="diff-nav-name">{name}</span>
                                    <span class="diff-nav-add">"+"{ add_count }</span>
                                    <span class="diff-nav-del">"-"{ del_count }</span>
                                </a>
                            }
                        }).collect_view()}
                    </div>
                }.into_view()
            } else {
                view! {}.into_view()
            }}

            // File diffs
            {files_for_body.into_iter().enumerate().map(|(fi, file)| {
                let anchor = format!("diff-file-{fi}");
                let name = if file.old_name == file.new_name {
                    file.new_name.clone()
                } else {
                    format!("{} → {}", file.old_name, file.new_name)
                };
                let hunks = file.hunks.clone();
                let mode = mode.clone();
                let collapsed = collapsed.clone();

                view! {
                    <div class="diff-file" id={anchor}>
                        <div class="diff-file-header">
                            <span class="diff-file-icon">"📄"</span>
                            <span class="diff-file-name">{name}</span>
                        </div>
                        <div class="diff-file-body">
                            {hunks.into_iter().enumerate().map(|(hi, hunk)| {
                                let header = hunk.header.clone();
                                let lines = hunk.lines.clone();
                                let mode = mode.clone();
                                let collapsed = collapsed.clone();
                                let fi = fi;
                                let hi = hi;

                                let is_collapsed = {
                                    let collapsed = collapsed.clone();
                                    move || collapsed.get().contains(&(fi, hi))
                                };

                                let toggle_collapse = {
                                    let collapsed = collapsed.clone();
                                    move |_: web_sys::MouseEvent| {
                                        collapsed.update(|c| {
                                            let key = (fi, hi);
                                            if let Some(pos) = c.iter().position(|k| *k == key) {
                                                c.remove(pos);
                                            } else {
                                                c.push(key);
                                            }
                                        });
                                    }
                                };

                                view! {
                                    <div class="diff-hunk">
                                        <div
                                            class="diff-hunk-header"
                                            on:click=toggle_collapse
                                            title="Click to expand/collapse"
                                        >
                                            <span class="diff-hunk-toggle">
                                                {move || if is_collapsed() { "▶" } else { "▼" }}
                                            </span>
                                            <code class="diff-hunk-range">{header.clone()}</code>
                                        </div>
                                        {move || {
                                            if is_collapsed() {
                                                return view! {
                                                    <div class="diff-hunk-collapsed">"… collapsed"</div>
                                                }.into_view();
                                            }

                                            let lines = lines.clone();
                                            if mode.get() == "split" {
                                                // Side-by-side
                                                let pairs = pair_lines(&lines);
                                                view! {
                                                    <div class="diff-sbs-table">
                                                        {pairs.into_iter().map(|pair| {
                                                            let (l_num, l_class, l_content) = match &pair.left {
                                                                Some(l) => {
                                                                    let cls = match l.kind {
                                                                        DiffLineKind::Del => "diff-line-del",
                                                                        DiffLineKind::Context => "diff-line-ctx",
                                                                        _ => "diff-line-empty",
                                                                    };
                                                                    let num = l.old_num.map(|n| n.to_string()).unwrap_or_default();
                                                                    (num, cls, l.content.clone())
                                                                }
                                                                None => (String::new(), "diff-line-empty", String::new()),
                                                            };
                                                            let (r_num, r_class, r_content) = match &pair.right {
                                                                Some(r) => {
                                                                    let cls = match r.kind {
                                                                        DiffLineKind::Add => "diff-line-add",
                                                                        DiffLineKind::Context => "diff-line-ctx",
                                                                        _ => "diff-line-empty",
                                                                    };
                                                                    let num = r.new_num.map(|n| n.to_string()).unwrap_or_default();
                                                                    (num, cls, r.content.clone())
                                                                }
                                                                None => (String::new(), "diff-line-empty", String::new()),
                                                            };
                                                            view! {
                                                                <div class="diff-sbs-row">
                                                                    <span class="diff-sbs-num">{l_num}</span>
                                                                    <pre class={format!("diff-sbs-code {l_class}")}>{l_content}</pre>
                                                                    <span class="diff-sbs-gutter"></span>
                                                                    <span class="diff-sbs-num">{r_num}</span>
                                                                    <pre class={format!("diff-sbs-code {r_class}")}>{r_content}</pre>
                                                                </div>
                                                            }
                                                        }).collect_view()}
                                                    </div>
                                                }.into_view()
                                            } else {
                                                // Unified
                                                view! {
                                                    <div class="diff-unified-table">
                                                        {lines.iter().map(|line| {
                                                            let (prefix, cls) = match line.kind {
                                                                DiffLineKind::Add => ("+", "diff-line-add"),
                                                                DiffLineKind::Del => ("-", "diff-line-del"),
                                                                DiffLineKind::Context => (" ", "diff-line-ctx"),
                                                            };
                                                            let old = line.old_num.map(|n| n.to_string()).unwrap_or_default();
                                                            let new = line.new_num.map(|n| n.to_string()).unwrap_or_default();
                                                            let content = line.content.clone();
                                                            view! {
                                                                <div class={format!("diff-uni-row {cls}")}>
                                                                    <span class="diff-uni-num diff-uni-num-old">{old}</span>
                                                                    <span class="diff-uni-num diff-uni-num-new">{new}</span>
                                                                    <span class="diff-uni-prefix">{prefix}</span>
                                                                    <pre class="diff-uni-code">{content}</pre>
                                                                </div>
                                                            }
                                                        }).collect_view()}
                                                    </div>
                                                }.into_view()
                                            }
                                        }}
                                    </div>
                                }
                            }).collect_view()}
                        </div>
                    </div>
                }
            }).collect_view()}
        </div>
    }
    .into_view()
}

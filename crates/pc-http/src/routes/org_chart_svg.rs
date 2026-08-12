//! 公司组织架构 SVG 渲染。

use axum::{
    extract::{Path, State},
    http::header,
    response::IntoResponse,
    routing::get,
    Router,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{state::require_user_id, AppState};
use pc_repos::agent::AgentRepo;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/companies/:company_id/org-chart.svg",
            get(org_chart_svg),
        )
        .route("/api/companies/:company_id/org.svg", get(org_chart_svg))
}

#[derive(Debug, Deserialize, Default)]
struct StyleQuery {
    style: Option<String>,
}

async fn org_chart_svg(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<StyleQuery>,
) -> impl IntoResponse {
    let _ = require_user_id(&state, &headers).await;
    let style = q.style.unwrap_or_else(|| "warmth".into());
    let pool = state.db.pool();
    let rows = AgentRepo::new(&state.db)
        .list_org_chart_simple(company_id)
        .await
        .unwrap_or_default();

    let svg = if rows.is_empty() {
        placeholder_svg(&style)
    } else {
        render_simple_svg(&rows, &style)
    };
    ([(header::CONTENT_TYPE, "image/svg+xml")], svg)
}

fn render_simple_svg(rows: &[(Uuid, String, String, String, Option<Uuid>)], style: &str) -> String {
    let t = theme(style);
    let width = 720_i32;
    let height = 480_i32;
    let mut cards = String::new();
    let mut lines = String::new();
    let cell_w = 160_i32;
    let cell_h = 80_i32;
    let margin = 30_i32;
    let cols = 3_i32;
    for (idx, (id, name, role, status, _reports_to)) in rows.iter().enumerate() {
        let cx = margin + ((idx as i32) % cols) * (cell_w + 20);
        let cy = margin + ((idx as i32) / cols) * (cell_h + 40);
        let fill = match status.as_str() {
            "running" => t.card_active,
            "paused" => t.card_paused,
            "error" => t.card_error,
            _ => t.card_default,
        };
        let status_color = match status.as_str() {
            "running" => "#22c55e",
            "paused" => "#f59e0b",
            "error" => "#ef4444",
            _ => "#94a3b8",
        };
        cards.push_str(&format!(
            "<g transform=\"translate({cx},{cy})\">\
             <rect width=\"{cw}\" height=\"{ch}\" rx=\"8\" fill=\"{fill}\" stroke=\"{border}\"/>\
             <text x=\"12\" y=\"22\" font-family=\"sans-serif\" font-size=\"14\" font-weight=\"700\" fill=\"{text}\">{name}</text>\
             <text x=\"12\" y=\"44\" font-family=\"sans-serif\" font-size=\"11\" fill=\"{subtext}\">{role}</text>\
             <circle cx=\"{scx}\" cy=\"14\" r=\"5\" fill=\"{sc}\"/>\
             <text x=\"{scx2}\" y=\"18\" font-family=\"sans-serif\" font-size=\"10\" fill=\"{subtext}\">{status}</text>\
             </g>",
            cw = cell_w,
            ch = cell_h,
            scx = cell_w - 70,
            scx2 = cell_w - 60,
            border = t.border,
            text = t.text,
            subtext = t.subtext,
            sc = status_color,
            name = html_escape(name),
            role = html_escape(role),
            status = html_escape(status),
        ));
        if idx > 0 {
            let prev_x = margin + (((idx - 1) as i32) % cols) * (cell_w + 20) + cell_w / 2;
            let prev_y = margin + (((idx - 1) as i32) / cols) * (cell_h + 40) + cell_h;
            lines.push_str(&format!(
                "<line x1=\"{prev_x}\" y1=\"{prev_y}\" x2=\"{cx}\" y2=\"{cy}\" stroke=\"{border}\" stroke-width=\"1.5\" opacity=\"0.4\"/>",
                border = t.border,
            ));
        }
        let _ = id;
    }
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {w} {h}\" width=\"{w}\" height=\"{h}\">\
         <rect width=\"100%\" height=\"100%\" fill=\"{bg}\" rx=\"6\"/>\
         <text x=\"20\" y=\"24\" font-family=\"sans-serif\" font-size=\"14\" font-weight=\"700\" fill=\"{text}\">Organization ({count})</text>\
         <text x=\"{w2}\" y=\"24\" font-family=\"sans-serif\" font-size=\"11\" fill=\"{subtext}\" text-anchor=\"end\">style: {style}</text>\
         {lines}\
         {cards}\
         </svg>",
        w = width,
        h = height,
        w2 = width - 20,
        count = rows.len(),
        bg = t.bg,
        text = t.text,
        subtext = t.subtext,
        style = style,
    )
}

struct Theme {
    bg: &'static str,
    text: &'static str,
    subtext: &'static str,
    border: &'static str,
    card_default: &'static str,
    card_active: &'static str,
    card_paused: &'static str,
    card_error: &'static str,
}

fn theme(style: &str) -> Theme {
    match style {
        "monochrome" => Theme {
            bg: "#0f172a",
            text: "#f8fafc",
            subtext: "#94a3b8",
            border: "#334155",
            card_default: "#1e293b",
            card_active: "#334155",
            card_paused: "#475569",
            card_error: "#1e293b",
        },
        "nebula" => Theme {
            bg: "#1e1b4b",
            text: "#f5d0fe",
            subtext: "#c4b5fd",
            border: "#6d28d9",
            card_default: "#312e81",
            card_active: "#4c1d95",
            card_paused: "#5b21b6",
            card_error: "#7f1d1d",
        },
        "circuit" => Theme {
            bg: "#0a0a0a",
            text: "#4ade80",
            subtext: "#22c55e",
            border: "#16a34a",
            card_default: "#052e16",
            card_active: "#14532d",
            card_paused: "#365314",
            card_error: "#7f1d1d",
        },
        "schematic" => Theme {
            bg: "#fafafa",
            text: "#0f172a",
            subtext: "#64748b",
            border: "#cbd5e1",
            card_default: "#ffffff",
            card_active: "#f0fdf4",
            card_paused: "#fef3c7",
            card_error: "#fee2e2",
        },
        _ => Theme {
            bg: "#fff7ed",
            text: "#7c2d12",
            subtext: "#9a3412",
            border: "#fdba74",
            card_default: "#fff7ed",
            card_active: "#fed7aa",
            card_paused: "#fdba74",
            card_error: "#fecaca",
        },
    }
}

fn placeholder_svg(style: &str) -> String {
    format!(
        "<svg xmlns='http://www.w3.org/2000/svg' width='320' height='120'>\
         <rect width='320' height='120' fill='#f5f5f5'/>\
         <text x='160' y='60' text-anchor='middle' fill='#666' font-family='sans-serif' font-size='14'>No agents in this company ({style})</text>\
         </svg>"
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

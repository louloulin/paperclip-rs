//! 公司组织架构 SVG 渲染。

use axum::{
    extract::{Path, State},
    http::header,
    response::IntoResponse,
    routing::get,
    Router,
};
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/companies/:company_id/org-chart.svg",
        get(org_chart_svg),
    )
}

const PLACEHOLDER_SVG: &str = "<svg xmlns='http://www.w3.org/2000/svg' width='320' height='120'>\
<rect width='320' height='120' fill='%23f5f5f5'/>\
<text x='160' y='60' text-anchor='middle' fill='%23666' font-family='sans-serif' font-size='14'>org-chart not implemented</text>\
</svg>";

async fn org_chart_svg(
    State(_state): State<AppState>,
    Path(_company_id): Path<Uuid>,
) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "image/svg+xml")],
        PLACEHOLDER_SVG.to_string(),
    )
}

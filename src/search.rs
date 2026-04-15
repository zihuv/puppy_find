use anyhow::{Result, anyhow};
use omni_search::cosine_similarity;
use serde::Serialize;

use crate::app_state::AppState;
use crate::db;

#[derive(Debug, Clone, Serialize)]
pub struct SearchItem {
    pub id: i64,
    pub score: f32,
    pub file_name: String,
    pub path: String,
    pub image_url: String,
}

pub fn run_search(state: &AppState, query: &str, limit: usize) -> Result<Vec<SearchItem>> {
    let settings = state.settings();
    if settings.model_dir.is_empty() || settings.image_dir.is_empty() {
        return Err(anyhow!("请先保存模型目录和图片目录"));
    }

    let query_vector = state
        .model_manager()
        .embed_text(settings.model_dir.as_ref(), query)?;
    let images = db::list_search_images(state.db_path())?;

    if images.is_empty() {
        return Err(anyhow!("请先建立索引"));
    }

    let mut items = Vec::with_capacity(images.len());
    for image in images {
        if image.dims != query_vector.len() || image.vector.len() != query_vector.len() {
            return Err(anyhow!("索引向量与当前模型不匹配，请重新建立索引"));
        }

        let score = cosine_similarity(&query_vector, &image.vector)
            .map_err(|error| anyhow!("failed to score image {}: {error}", image.path))?;

        items.push(SearchItem {
            id: image.id,
            score,
            file_name: image.file_name,
            path: image.path,
            image_url: format!("/api/images/{}", image.id),
        });
    }

    items.sort_by(|left, right| right.score.total_cmp(&left.score));
    items.truncate(limit.min(items.len()));

    Ok(items)
}

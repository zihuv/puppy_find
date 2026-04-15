use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::db::AppSettings;
use crate::model::ModelManager;

#[derive(Clone)]
pub struct AppState {
    db_path: Arc<PathBuf>,
    settings: Arc<Mutex<AppSettings>>,
    index_status: Arc<Mutex<IndexStatus>>,
    model_manager: ModelManager,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct IndexStatus {
    pub running: bool,
    pub total: usize,
    pub processed: usize,
    pub current_file: Option<String>,
    pub error: Option<String>,
}

impl AppState {
    pub fn new(db_path: PathBuf, settings: AppSettings) -> Self {
        Self {
            db_path: Arc::new(db_path),
            settings: Arc::new(Mutex::new(settings)),
            index_status: Arc::new(Mutex::new(IndexStatus::default())),
            model_manager: ModelManager::default(),
        }
    }

    pub fn db_path(&self) -> &Path {
        self.db_path.as_ref().as_path()
    }

    pub fn settings(&self) -> AppSettings {
        self.settings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn replace_settings(&self, settings: AppSettings) {
        *self
            .settings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = settings;
    }

    pub fn model_manager(&self) -> &ModelManager {
        &self.model_manager
    }

    pub fn index_status(&self) -> IndexStatus {
        self.index_status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn update_index_status(&self, update: impl FnOnce(&mut IndexStatus)) {
        let mut status = self
            .index_status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        update(&mut status);
    }

    pub fn try_start_indexing(&self) -> bool {
        let mut status = self
            .index_status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if status.running {
            return false;
        }

        *status = IndexStatus {
            running: true,
            total: 0,
            processed: 0,
            current_file: None,
            error: None,
        };
        true
    }

    pub fn finish_indexing(&self, error: Option<String>) {
        self.update_index_status(|status| {
            status.running = false;
            status.current_file = None;
            status.error = error;
        });
    }
}

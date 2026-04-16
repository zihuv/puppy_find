use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::config::{self, AppSettings};
use crate::model::ModelManager;

#[derive(Clone)]
pub struct AppState {
    workspace_dir: Arc<PathBuf>,
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
    pub fn new(workspace_dir: PathBuf, settings: AppSettings) -> Self {
        Self {
            workspace_dir: Arc::new(workspace_dir),
            settings: Arc::new(Mutex::new(settings)),
            index_status: Arc::new(Mutex::new(IndexStatus::default())),
            model_manager: ModelManager::default(),
        }
    }

    pub fn workspace_dir(&self) -> &Path {
        self.workspace_dir.as_ref().as_path()
    }

    pub fn db_path(&self) -> PathBuf {
        let settings = self.settings();
        config::resolve_path(self.workspace_dir(), &settings.db_path)
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

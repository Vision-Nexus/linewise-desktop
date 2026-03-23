use dioxus::prelude::*;
use lw_core::models::{Project, Tenant, UploadTask, UserInfo};

#[derive(Clone)]
#[allow(dead_code)]
pub struct AppState {
    pub is_authenticated: Signal<bool>,
    pub user_info: Signal<Option<UserInfo>>,
    pub selected_tenant: Signal<Option<Tenant>>,
    pub selected_project: Signal<Option<Project>>,
    pub upload_tasks: Signal<Vec<UploadTask>>,
    pub is_loading: Signal<bool>,
    pub error_message: Signal<Option<String>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            is_authenticated: Signal::new(false),
            user_info: Signal::new(None),
            selected_tenant: Signal::new(None),
            selected_project: Signal::new(None),
            upload_tasks: Signal::new(Vec::new()),
            is_loading: Signal::new(false),
            error_message: Signal::new(None),
        }
    }
}

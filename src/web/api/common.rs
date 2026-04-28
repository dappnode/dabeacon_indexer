use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub(super) struct PaginatedResponse<T: Serialize> {
    pub data: Vec<T>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub(super) struct PaginationParams {
    pub order: String,
    pub page: String,
    pub per_page: String,
}

impl Default for PaginationParams {
    fn default() -> Self {
        Self {
            order: default_order(),
            page: default_page(),
            per_page: default_per_page(),
        }
    }
}

impl PaginationParams {
    pub fn page_num(&self) -> i64 {
        self.page
            .parse::<i64>()
            .ok()
            .filter(|v| *v > 0)
            .unwrap_or(1)
    }

    pub fn per_page_num(&self) -> i64 {
        self.per_page
            .parse::<i64>()
            .ok()
            .filter(|v| *v > 0)
            .unwrap_or(50)
    }
}

pub(super) fn default_page() -> String {
    "1".to_string()
}

pub(super) fn default_per_page() -> String {
    "50".to_string()
}

pub(super) fn default_order() -> String {
    "desc".to_string()
}

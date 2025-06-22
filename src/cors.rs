use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorsConfig {
    pub allowed_origins: Vec<String>,
    pub allowed_methods: Vec<String>,
    pub allowed_headers: Vec<String>,
    pub expose_headers: Vec<String>,
    pub max_age: Option<u32>,
    pub allow_credentials: bool,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            allowed_origins: vec!["*".to_string()],
            allowed_methods: vec!["GET".to_string(), "OPTIONS".to_string()],
            allowed_headers: vec!["*".to_string()],
            expose_headers: vec![],
            max_age: Some(86400), // 24 小时
            allow_credentials: false,
        }
    }
}

impl CorsConfig {
    pub fn apply_headers(&self, headers: &mut HeaderMap) {
        // Access-Control-Allow-Origin
        if let Some(origin) = self.allowed_origins.first() {
            headers.insert("access-control-allow-origin", origin.parse().unwrap());
        }
        
        // Access-Control-Allow-Methods
        let methods = self.allowed_methods.join(", ");
        headers.insert("access-control-allow-methods", methods.parse().unwrap());
        
        // Access-Control-Allow-Headers
        let allowed_headers = self.allowed_headers.join(", ");
        headers.insert("access-control-allow-headers", allowed_headers.parse().unwrap());
        
        // Access-Control-Expose-Headers
        if !self.expose_headers.is_empty() {
            let expose_headers = self.expose_headers.join(", ");
            headers.insert("access-control-expose-headers", expose_headers.parse().unwrap());
        }
        
        // Access-Control-Max-Age
        if let Some(max_age) = self.max_age {
            headers.insert("access-control-max-age", max_age.to_string().parse().unwrap());
        }
        
        // Access-Control-Allow-Credentials
        if self.allow_credentials {
            headers.insert("access-control-allow-credentials", "true".parse().unwrap());
        }
    }
} 
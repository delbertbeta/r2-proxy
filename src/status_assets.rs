pub const INDEX_HTML: &str = include_str!("../status/index.html");
pub const APP_CSS: &str = include_str!("../status/app.css");
pub const APP_JS: &str = include_str!("../status/app.js");

pub fn asset(path: &str) -> Option<(&'static str, &'static [u8])> {
    match path {
        "/" | "/index.html" => Some(("text/html; charset=utf-8", INDEX_HTML.as_bytes())),
        "/assets/app.css" => Some(("text/css; charset=utf-8", APP_CSS.as_bytes())),
        "/assets/app.js" => Some(("application/javascript; charset=utf-8", APP_JS.as_bytes())),
        _ => None,
    }
}

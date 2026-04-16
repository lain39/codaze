use axum::http::{HeaderMap, HeaderName, header::CACHE_CONTROL, header::CONTENT_TYPE};

pub(crate) fn shape_openai_http_headers(headers: HeaderMap) -> HeaderMap {
    let mut filtered = HeaderMap::new();
    copy_header_if_present(&headers, &mut filtered, CONTENT_TYPE.as_str());
    copy_header_if_present(&headers, &mut filtered, CACHE_CONTROL.as_str());
    filtered
}

fn copy_header_if_present(source: &HeaderMap, dest: &mut HeaderMap, name: &str) {
    let Ok(header_name) = HeaderName::from_bytes(name.as_bytes()) else {
        return;
    };
    for value in source.get_all(&header_name) {
        dest.append(header_name.clone(), value.clone());
    }
}

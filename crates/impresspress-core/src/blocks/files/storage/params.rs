//! Path-parameter extraction for the storage API: bucket name and object
//! key. Both prefer the router-bound `{name}`/`{key...}` path vars, falling
//! back to a prefix-strip for the admin delegation path and hand-built
//! test messages.

use wafer_run::Message;

/// Extract the bucket name. Prefers the matcher-bound `{name}` path var, with a
/// prefix-strip fallback for the admin delegation path and hand-built tests.
pub(super) fn extract_bucket_name(msg: &Message) -> String {
    let var = msg.var("name");
    if !var.is_empty() {
        return var.to_string();
    }
    let path = msg.path();
    let rest = path
        .strip_prefix("/b/storage/api/buckets/")
        .or_else(|| path.strip_prefix("/admin/storage/buckets/"))
        .unwrap_or("");
    match rest.find('/') {
        Some(idx) => rest[..idx].to_string(),
        None => rest.to_string(),
    }
}

/// Extract the object key (may contain `/`). Prefers the matcher-bound
/// `{key...}` rest param, falling back to the substring after `/objects/`.
pub(super) fn extract_object_key(msg: &Message) -> String {
    let var = msg.var("key");
    if !var.is_empty() {
        return var.to_string();
    }
    let path = msg.path();
    match path.find("/objects/") {
        Some(idx) => path[idx + 9..].to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a message carrying `path` on `req.resource` and, optionally, the
    /// matcher-bound `{name}`/`{key}` path vars in `req.param.*`.
    fn msg_with(path: &str, params: &[(&str, &str)]) -> Message {
        let mut m = Message::new("test");
        m.set_meta("req.resource", path);
        for (k, v) in params {
            m.set_meta(format!("req.param.{k}"), *v);
        }
        m
    }

    #[test]
    fn test_extract_bucket_name_from_param() {
        // Router-populated path var wins (the normal dispatch path).
        let m = msg_with(
            "/b/storage/api/buckets/my-bucket/objects",
            &[("name", "my-bucket")],
        );
        assert_eq!(extract_bucket_name(&m), "my-bucket");
    }

    #[test]
    fn test_extract_bucket_name_prefix_fallback() {
        // Fallback for the admin delegation path / hand-built messages.
        assert_eq!(
            extract_bucket_name(&msg_with("/b/storage/api/buckets/my-bucket", &[])),
            "my-bucket"
        );
        assert_eq!(
            extract_bucket_name(&msg_with("/b/storage/api/buckets/my-bucket/objects", &[])),
            "my-bucket"
        );
        assert_eq!(
            extract_bucket_name(&msg_with("/admin/storage/buckets/admin-bucket", &[])),
            "admin-bucket"
        );
        assert_eq!(extract_bucket_name(&msg_with("/other/path", &[])), "");
    }

    #[test]
    fn test_extract_object_key_from_param() {
        // Rest param preserves embedded slashes.
        let m = msg_with(
            "/b/storage/api/buckets/b/objects/dir/file.txt",
            &[("key", "dir/file.txt")],
        );
        assert_eq!(extract_object_key(&m), "dir/file.txt");
    }

    #[test]
    fn test_extract_object_key_prefix_fallback() {
        assert_eq!(
            extract_object_key(&msg_with("/b/storage/api/buckets/b/objects/file.txt", &[])),
            "file.txt"
        );
        assert_eq!(
            extract_object_key(&msg_with(
                "/b/storage/api/buckets/b/objects/dir/file.txt",
                &[]
            )),
            "dir/file.txt"
        );
        assert_eq!(
            extract_object_key(&msg_with("/b/storage/api/buckets/b", &[])),
            ""
        );
        assert_eq!(extract_object_key(&msg_with("/other/path", &[])), "");
    }
}

use uuid::Uuid;

fn new_prefixed_id(prefix: &str) -> String {
    let value = Uuid::new_v4().simple().to_string();
    format!("{prefix}_{}", &value[..8])
}

pub fn new_project_id() -> String {
    new_prefixed_id("prj")
}

pub fn new_server_id() -> String {
    new_prefixed_id("srv")
}

pub fn new_temporary_id() -> String {
    new_prefixed_id("tmp")
}

fn has_id_shape(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|suffix| {
        suffix.len() == 8
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

pub fn is_project_id(value: &str) -> bool {
    has_id_shape(value, "prj_")
}

pub fn is_server_id(value: &str) -> bool {
    has_id_shape(value, "srv_")
}

pub fn is_temporary_id(value: &str) -> bool {
    has_id_shape(value, "tmp_")
}

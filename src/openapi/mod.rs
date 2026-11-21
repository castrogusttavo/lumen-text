//! OpenAPI / Swagger support: the pure logic behind the API explorer (`app_openapi`). A small
//! ordered JSON/YAML tree (`tree`), the spec model (`spec`), `$ref` and schema helpers
//! (`schema`), example generation (`example`), response validation (`validate`), the spec lint
//! (`lint`), the request builder (`request`) and the code snippets (`snippets`).

pub mod example;
pub mod lint;
pub mod request;
pub mod schema;
pub mod snippets;
pub mod spec;
pub mod tree;
pub mod validate;

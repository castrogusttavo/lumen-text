//! The OpenAPI / Swagger model: info, servers, tags, operations, security schemes. Schemas stay
//! in the raw tree (see `schema`); the operations hold clones of the schema nodes they use.

use super::schema::deref;
use super::tree::{Node, Val, parse, scalar_text};
use std::collections::HashMap;

const METHODS: [&str; 8] = ["get", "put", "post", "delete", "options", "head", "patch", "trace"];

#[derive(Clone, Debug)]
pub struct Server {
    pub url: String,
    pub description: String,
}

#[derive(Clone, Debug)]
pub struct Tag {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    /// `path`, `query`, `header`, `cookie`, `formData`.
    pub loc: String,
    pub required: bool,
    pub description: String,
    pub schema: Node,
    pub example: Option<Node>,
    pub deprecated: bool,
}

#[derive(Clone, Debug)]
pub struct Media {
    pub content_type: String,
    pub schema: Option<Node>,
    pub example: Option<Node>,
}

#[derive(Clone, Debug)]
pub struct Body {
    pub required: bool,
    pub description: String,
    pub media: Vec<Media>,
}

#[derive(Clone, Debug)]
pub struct Response {
    pub code: String,
    pub description: String,
    pub media: Vec<Media>,
    pub headers: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Operation {
    pub method: String,
    pub path: String,
    pub op_id: String,
    pub summary: String,
    pub description: String,
    pub tags: Vec<String>,
    pub params: Vec<Param>,
    pub body: Option<Body>,
    pub responses: Vec<Response>,
    pub deprecated: bool,
    /// Names of the security schemes that apply (operation level, else global).
    pub security: Vec<String>,
    pub line: usize,
    /// Lower-case text the search box matches against.
    pub hay: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecKind {
    Bearer,
    Basic,
    /// `in` is `header`, `query` or `cookie`.
    ApiKey {
        name: String,
        loc: String,
    },
    OAuth2,
    Other,
}

#[derive(Clone, Debug)]
pub struct SecScheme {
    pub name: String,
    pub kind: SecKind,
    pub description: String,
}

#[derive(Clone, Debug)]
pub struct Spec {
    pub root: Node,
    pub swagger2: bool,
    pub version: String,
    pub title: String,
    pub api_version: String,
    pub description: String,
    pub servers: Vec<Server>,
    pub tags: Vec<Tag>,
    pub ops: Vec<Operation>,
    pub schemes: Vec<SecScheme>,
    /// `components.schemas` (or `definitions`): name, line.
    pub schemas: Vec<(String, usize)>,
    pub global_security: Vec<String>,
}

/// A navigation group: tag name, description and the operations under it.
#[derive(Clone, Debug)]
pub struct Group {
    pub name: String,
    pub description: String,
    pub ops: Vec<usize>,
}

/// True when `text` looks like an OpenAPI / Swagger document (looks at the first lines only).
pub fn sniff(text: &str) -> bool {
    text.lines().take(60).any(|l| {
        let t = l.trim_start().trim_start_matches('{').trim_start().trim_end_matches(',');
        let key = |k: &str| {
            t.strip_prefix(k).is_some_and(|r| r.trim_start().starts_with(':'))
                || t.strip_prefix(&format!("\"{k}\"")).is_some_and(|r| r.trim_start().starts_with(':'))
        };
        key("openapi") || key("swagger")
    })
}

impl Spec {
    pub fn parse(text: &str) -> Result<Spec, String> {
        let root = parse(text)?;
        Spec::from_tree(root)
    }

    pub fn from_tree(root: Node) -> Result<Spec, String> {
        if !root.is_map() {
            return Err("the document is not an object".into());
        }
        let swagger2 = root.get("swagger").is_some();
        let version = match root.get("openapi").or_else(|| root.get("swagger")) {
            Some(n) => match &n.val {
                Val::Num(x) if *x == 2.0 => "2.0".to_string(),
                _ => scalar_text(n),
            },
            None => return Err("not an OpenAPI document: no 'openapi' or 'swagger' key".into()),
        };
        let info = root.get("info");
        let g = |k: &str| info.and_then(|i| i.gets(k)).unwrap_or("").to_string();
        let title = if g("title").is_empty() { "API".to_string() } else { g("title") };
        let api_version = info.and_then(|i| i.get("version")).map(scalar_text).unwrap_or_default();
        let mut spec = Spec {
            swagger2,
            version,
            title,
            api_version,
            description: g("description"),
            servers: servers(&root, swagger2),
            tags: root
                .get("tags")
                .map(|t| t.arr())
                .unwrap_or(&[])
                .iter()
                .filter_map(|t| {
                    let name = t.gets("name")?.to_string();
                    Some(Tag { name, description: t.gets("description").unwrap_or("").to_string() })
                })
                .collect(),
            ops: Vec::new(),
            schemes: Vec::new(),
            schemas: Vec::new(),
            global_security: security_names(root.get("security")),
            root: Node::new(Val::Null, 0),
        };
        spec.schemes = schemes(&root, swagger2);
        let defs =
            if swagger2 { root.get("definitions") } else { root.get("components").and_then(|c| c.get("schemas")) };
        spec.schemas = defs.map(|d| d.entries().iter().map(|(k, v)| (k.clone(), v.line)).collect()).unwrap_or_default();
        spec.ops = operations(&root, swagger2, &spec.global_security);
        spec.root = root;
        Ok(spec)
    }

    /// Operations grouped by their first tag: declared tags first (in order), then the others.
    pub fn groups(&self) -> Vec<Group> {
        let mut out: Vec<Group> = self
            .tags
            .iter()
            .map(|t| Group { name: t.name.clone(), description: t.description.clone(), ops: vec![] })
            .collect();
        let mut index: HashMap<String, usize> = out.iter().enumerate().map(|(i, g)| (g.name.clone(), i)).collect();
        for (i, op) in self.ops.iter().enumerate() {
            let name = op.tags.first().cloned().unwrap_or_else(|| "Operations".to_string());
            let gi = *index.entry(name.clone()).or_insert_with(|| {
                out.push(Group { name, description: String::new(), ops: vec![] });
                out.len() - 1
            });
            out[gi].ops.push(i);
        }
        out.retain(|g| !g.ops.is_empty());
        out
    }

    pub fn scheme(&self, name: &str) -> Option<&SecScheme> {
        self.schemes.iter().find(|s| s.name == name)
    }
}

fn servers(root: &Node, swagger2: bool) -> Vec<Server> {
    if swagger2 {
        let host = root.gets("host").unwrap_or("");
        if host.is_empty() {
            return Vec::new();
        }
        let scheme = root.get("schemes").and_then(|s| s.arr().first()).and_then(|s| s.str()).unwrap_or("https");
        let base = root.gets("basePath").unwrap_or("");
        return vec![Server { url: format!("{scheme}://{host}{base}"), description: String::new() }];
    }
    root.get("servers")
        .map(|s| s.arr())
        .unwrap_or(&[])
        .iter()
        .filter_map(|s| {
            let mut url = s.gets("url")?.to_string();
            for (name, v) in s.get("variables").map(|v| v.entries()).unwrap_or(&[]) {
                let def = v.get("default").map(scalar_text).unwrap_or_default();
                url = url.replace(&format!("{{{name}}}"), &def);
            }
            Some(Server { url, description: s.gets("description").unwrap_or("").to_string() })
        })
        .collect()
}

fn schemes(root: &Node, swagger2: bool) -> Vec<SecScheme> {
    let defs = if swagger2 {
        root.get("securityDefinitions")
    } else {
        root.get("components").and_then(|c| c.get("securitySchemes"))
    };
    defs.map(|d| d.entries())
        .unwrap_or(&[])
        .iter()
        .map(|(name, raw)| {
            let s = deref(root, raw);
            let ty = s.gets("type").unwrap_or("");
            let kind = match ty {
                "http" => match s.gets("scheme").unwrap_or("").to_ascii_lowercase().as_str() {
                    "bearer" => SecKind::Bearer,
                    "basic" => SecKind::Basic,
                    _ => SecKind::Other,
                },
                "basic" => SecKind::Basic,
                "apiKey" => SecKind::ApiKey {
                    name: s.gets("name").unwrap_or("").to_string(),
                    loc: s.gets("in").unwrap_or("header").to_string(),
                },
                "oauth2" | "openIdConnect" => SecKind::OAuth2,
                _ => SecKind::Other,
            };
            SecScheme { name: name.clone(), kind, description: s.gets("description").unwrap_or("").to_string() }
        })
        .collect()
}

fn security_names(n: Option<&Node>) -> Vec<String> {
    let mut out = Vec::new();
    for req in n.map(|n| n.arr()).unwrap_or(&[]) {
        for (k, _) in req.entries() {
            if !out.contains(k) {
                out.push(k.clone());
            }
        }
    }
    out
}

fn example_of(n: &Node) -> Option<Node> {
    if let Some(e) = n.get("example") {
        return Some(e.clone());
    }
    let ex = n.get("examples")?;
    let first = ex.entries().first().map(|(_, v)| v).or_else(|| ex.arr().first())?;
    Some(first.get("value").unwrap_or(first).clone())
}

fn media_map(root: &Node, content: Option<&Node>) -> Vec<Media> {
    content
        .map(|c| c.entries())
        .unwrap_or(&[])
        .iter()
        .map(|(ct, m)| {
            let m = deref(root, m);
            Media { content_type: ct.clone(), schema: m.get("schema").cloned(), example: example_of(m) }
        })
        .collect()
}

fn param_of(root: &Node, raw: &Node, swagger2: bool) -> Option<Param> {
    let p = deref(root, raw);
    let name = p.gets("name")?.to_string();
    let loc = p.gets("in")?.to_string();
    let schema = match p.get("schema") {
        Some(s) => s.clone(),
        None if swagger2 => p.clone(),
        None => Node::new(Val::Map(vec![]), p.line),
    };
    let example = example_of(p).or_else(|| deref(root, &schema).get("example").cloned());
    Some(Param {
        name,
        loc,
        required: p.get("required").and_then(|r| r.bool()).unwrap_or(false),
        description: p.gets("description").unwrap_or("").to_string(),
        schema,
        example,
        deprecated: p.get("deprecated").and_then(|r| r.bool()).unwrap_or(false),
    })
}

fn operations(root: &Node, swagger2: bool, global_sec: &[String]) -> Vec<Operation> {
    let mut out = Vec::new();
    let Some(paths) = root.get("paths") else { return out };
    let default_consumes: Vec<String> = root
        .get("consumes")
        .map(|c| c.arr().iter().filter_map(|x| x.str().map(String::from)).collect())
        .unwrap_or_default();
    let default_produces: Vec<String> = root
        .get("produces")
        .map(|c| c.arr().iter().filter_map(|x| x.str().map(String::from)).collect())
        .unwrap_or_default();
    for (path, item_raw) in paths.entries() {
        let item = deref(root, item_raw);
        let shared: Vec<Param> = item
            .get("parameters")
            .map(|p| p.arr())
            .unwrap_or(&[])
            .iter()
            .filter_map(|p| param_of(root, p, swagger2))
            .collect();
        for (key, op) in item.entries() {
            let method = key.to_ascii_lowercase();
            if !METHODS.contains(&method.as_str()) || !op.is_map() {
                continue;
            }
            let mut params: Vec<Param> = shared.clone();
            let mut body: Option<Body> = None;
            for raw in op.get("parameters").map(|p| p.arr()).unwrap_or(&[]) {
                let d = deref(root, raw);
                if swagger2 && d.gets("in") == Some("body") {
                    let cts = op
                        .get("consumes")
                        .map(|c| c.arr().iter().filter_map(|x| x.str().map(String::from)).collect::<Vec<_>>())
                        .unwrap_or_else(|| default_consumes.clone());
                    let cts = if cts.is_empty() { vec!["application/json".to_string()] } else { cts };
                    body = Some(Body {
                        required: d.get("required").and_then(|r| r.bool()).unwrap_or(false),
                        description: d.gets("description").unwrap_or("").to_string(),
                        media: cts
                            .into_iter()
                            .map(|ct| Media {
                                content_type: ct,
                                schema: d.get("schema").cloned(),
                                example: example_of(d),
                            })
                            .collect(),
                    });
                    continue;
                }
                if let Some(p) = param_of(root, raw, swagger2) {
                    params.retain(|q| !(q.name == p.name && q.loc == p.loc));
                    params.push(p);
                }
            }
            if let Some(rb) = op.get("requestBody") {
                let rb = deref(root, rb);
                body = Some(Body {
                    required: rb.get("required").and_then(|r| r.bool()).unwrap_or(false),
                    description: rb.gets("description").unwrap_or("").to_string(),
                    media: media_map(root, rb.get("content")),
                });
            }
            let produces: Vec<String> = op
                .get("produces")
                .map(|c| c.arr().iter().filter_map(|x| x.str().map(String::from)).collect())
                .unwrap_or_else(|| default_produces.clone());
            let mut responses = Vec::new();
            for (code, raw) in op.get("responses").map(|r| r.entries()).unwrap_or(&[]) {
                let r = deref(root, raw);
                let mut media = media_map(root, r.get("content"));
                if swagger2 && let Some(s) = r.get("schema") {
                    let cts = if produces.is_empty() { vec!["application/json".to_string()] } else { produces.clone() };
                    for ct in cts {
                        let example = r.get("examples").and_then(|e| e.get(&ct)).cloned();
                        media.push(Media { content_type: ct, schema: Some(s.clone()), example });
                    }
                }
                responses.push(Response {
                    code: code.clone(),
                    description: r.gets("description").unwrap_or("").to_string(),
                    media,
                    headers: r
                        .get("headers")
                        .map(|h| h.entries().iter().map(|(k, _)| k.clone()).collect())
                        .unwrap_or_default(),
                });
            }
            let security = match op.get("security") {
                Some(s) => security_names(Some(s)),
                None => global_sec.to_vec(),
            };
            let tags: Vec<String> = op
                .get("tags")
                .map(|t| t.arr().iter().filter_map(|x| x.str().map(String::from)).collect())
                .unwrap_or_default();
            let summary = op.gets("summary").unwrap_or("").to_string();
            let description = op.gets("description").unwrap_or("").to_string();
            let op_id = op.gets("operationId").unwrap_or("").to_string();
            let hay = format!("{method} {path} {summary} {op_id} {} {}", tags.join(" "), description).to_lowercase();
            out.push(Operation {
                method,
                path: path.clone(),
                op_id,
                summary,
                description,
                tags,
                params,
                body,
                responses,
                deprecated: op.get("deprecated").and_then(|d| d.bool()).unwrap_or(false),
                security,
                line: op.line,
                hay,
            });
        }
    }
    out
}

/// A tiny petstore used by the tests of every `openapi` module.
#[cfg(test)]
pub const PETSTORE: &str = r##"openapi: 3.0.3
info:
  title: Petstore
  version: 1.0.0
  description: A **small** store.
servers:
  - url: https://api.example.com/v1
    description: Production
tags:
  - name: pets
    description: Everything about pets
paths:
  /pets:
    get:
      tags: [pets]
      summary: List pets
      operationId: listPets
      parameters:
        - name: limit
          in: query
          schema:
            type: integer
            format: int32
            maximum: 100
      responses:
        '200':
          description: A list of pets
          content:
            application/json:
              schema:
                type: array
                items:
                  $ref: '#/components/schemas/Pet'
    post:
      tags: [pets]
      summary: Create a pet
      operationId: createPet
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: '#/components/schemas/NewPet'
      responses:
        '201':
          description: Created
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/Pet'
        default:
          description: Error
  /pets/{petId}:
    parameters:
      - name: petId
        in: path
        required: true
        schema:
          type: string
    get:
      tags: [pets]
      summary: Info for a pet
      operationId: showPetById
      security:
        - bearerAuth: []
      responses:
        '200':
          description: Expected response
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/Pet'
components:
  securitySchemes:
    bearerAuth:
      type: http
      scheme: bearer
    sessionCookie:
      type: apiKey
      in: cookie
      name: sid
  schemas:
    NewPet:
      type: object
      required: [name]
      properties:
        name:
          type: string
        tag:
          type: string
          enum: [cat, dog]
    Pet:
      allOf:
        - $ref: '#/components/schemas/NewPet'
        - type: object
          required: [id]
          properties:
            id:
              type: integer
              format: int64
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn petstore_model() {
        let s = Spec::parse(PETSTORE).unwrap();
        assert_eq!((s.title.as_str(), s.api_version.as_str(), s.version.as_str()), ("Petstore", "1.0.0", "3.0.3"));
        assert_eq!(s.servers[0].url, "https://api.example.com/v1");
        assert_eq!(s.ops.len(), 3);
        let show = &s.ops[2];
        assert_eq!((show.method.as_str(), show.path.as_str()), ("get", "/pets/{petId}"));
        assert_eq!(show.params[0].name, "petId", "path level parameters are inherited");
        assert!(show.params[0].required);
        assert_eq!(show.security, ["bearerAuth"]);
        let create = &s.ops[1];
        assert!(create.body.as_ref().unwrap().required);
        assert_eq!(create.responses.len(), 2);
        assert_eq!(s.schemas.len(), 2);
        assert_eq!(s.schemes[1].kind, SecKind::ApiKey { name: "sid".into(), loc: "cookie".into() });
        let g = s.groups();
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].ops, [0, 1, 2]);
    }

    #[test]
    fn sniff_finds_json_and_yaml_markers() {
        assert!(sniff("openapi: 3.0.0\n"));
        assert!(sniff("{\n  \"openapi\": \"3.1.0\",\n"));
        assert!(sniff("{\"swagger\": \"2.0\""));
        assert!(!sniff("{\"name\": \"x\", \"openapi_like\": 1}"));
        assert!(!sniff("name: x\nversion: 1\n"));
    }

    #[test]
    fn swagger2_is_normalised() {
        let s = Spec::parse(
            "swagger: '2.0'\ninfo: {title: Old, version: '1'}\nhost: h.example.com\nbasePath: /api\nschemes: [https]\npaths:\n  /x:\n    post:\n      parameters:\n        - {name: b, in: body, required: true, schema: {type: object}}\n        - {name: q, in: query, type: string}\n      responses:\n        '200': {description: ok, schema: {type: string}}\n",
        )
        .unwrap();
        assert!(s.swagger2);
        assert_eq!(s.servers[0].url, "https://h.example.com/api");
        let op = &s.ops[0];
        assert_eq!(op.body.as_ref().unwrap().media[0].content_type, "application/json");
        assert_eq!(op.params.len(), 1);
        assert_eq!(op.responses[0].media[0].schema.as_ref().unwrap().gets("type"), Some("string"));
    }

    #[test]
    fn rejects_other_documents() {
        assert!(Spec::parse("{\"a\": 1}").is_err());
        assert!(Spec::parse("- a\n- b\n").is_err());
    }
}

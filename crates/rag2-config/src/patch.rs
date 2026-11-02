//! JSON merge patches (RFC 7386) for environment overlays, and structural diffs for change previews.

use serde::Serialize;
use serde_json::Value;

/// Apply `patch` to `target` per RFC 7386: objects merge recursively, `null` deletes, anything
/// else replaces.
pub fn merge_patch(target: &Value, patch: &Value) -> Value {
    let Value::Object(p) = patch else {
        return patch.clone();
    };
    let mut out = match target {
        Value::Object(t) => t.clone(),
        _ => serde_json::Map::new(),
    };
    for (k, v) in p {
        if v.is_null() {
            out.remove(k);
        } else {
            let merged = merge_patch(out.get(k).unwrap_or(&Value::Null), v);
            out.insert(k.clone(), merged);
        }
    }
    Value::Object(out)
}

#[derive(Debug, Clone, PartialEq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", tag = "op")]
pub enum Change {
    Add {
        path: String,
        value: Value,
    },
    Remove {
        path: String,
        value: Value,
    },
    Replace {
        path: String,
        before: Value,
        after: Value,
    },
}

/// Leaf-level differences from `a` to `b`, paths in dotted form (`pipelines.default.top_k`,
/// arrays as `retrievers[0]`).
pub fn diff(a: &Value, b: &Value) -> Vec<Change> {
    let mut out = Vec::new();
    walk("", a, b, &mut out);
    out
}

fn join(base: &str, key: &str) -> String {
    if base.is_empty() {
        key.to_string()
    } else {
        format!("{base}.{key}")
    }
}

fn walk(path: &str, a: &Value, b: &Value, out: &mut Vec<Change>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for (k, va) in x {
                match y.get(k) {
                    Some(vb) => walk(&join(path, k), va, vb, out),
                    None => out.push(Change::Remove {
                        path: join(path, k),
                        value: va.clone(),
                    }),
                }
            }
            for (k, vb) in y {
                if !x.contains_key(k) {
                    out.push(Change::Add {
                        path: join(path, k),
                        value: vb.clone(),
                    });
                }
            }
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (i, (va, vb)) in x.iter().zip(y).enumerate() {
                walk(&format!("{path}[{i}]"), va, vb, out);
            }
        }
        _ if a != b => out.push(Change::Replace {
            path: path.to_string(),
            before: a.clone(),
            after: b.clone(),
        }),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_patch_follows_rfc_7386() {
        let t = json!({"a": "b", "c": {"d": "e", "f": "g"}});
        let p = json!({"a": "z", "c": {"f": null}});
        assert_eq!(merge_patch(&t, &p), json!({"a": "z", "c": {"d": "e"}}));
        assert_eq!(
            merge_patch(&json!({"a": [1]}), &json!({"a": [2, 3]})),
            json!({"a": [2, 3]})
        );
    }

    #[test]
    fn diff_reports_leaf_changes_with_paths() {
        let a = json!({"x": 1, "p": {"top_k": 10, "r": [{"w": 1.0}]}, "gone": true});
        let b = json!({"x": 1, "p": {"top_k": 20, "r": [{"w": 2.0}]}, "new": "y"});
        let d = diff(&a, &b);
        assert!(d.contains(&Change::Replace {
            path: "p.top_k".into(),
            before: json!(10),
            after: json!(20)
        }));
        assert!(d.contains(&Change::Replace {
            path: "p.r[0].w".into(),
            before: json!(1.0),
            after: json!(2.0)
        }));
        assert!(d.contains(&Change::Remove {
            path: "gone".into(),
            value: json!(true)
        }));
        assert!(d.contains(&Change::Add {
            path: "new".into(),
            value: json!("y")
        }));
        assert_eq!(d.len(), 4);
    }
}
